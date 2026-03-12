//! hypr-overlay-wl — phase 2: EGL/GLES2 hardware-accelerated layer-shell overlay.
//!
//! Transparent except for two rounded buttons (Mute/Deafen) and a drag handle.
//! Click-through everywhere else via wl_surface.set_input_region.
//! Discord IPC: set DISCORD_CLIENT_ID + DISCORD_CLIENT_SECRET to enable.

mod config;
mod discord;
mod render;
mod state;
mod handlers;

use std::sync::mpsc;

use calloop::EventLoop;
use calloop_wayland_source::WaylandSource;
use smithay_client_toolkit as sctk;
use sctk::compositor::CompositorState;
use sctk::output::OutputState;
use sctk::reexports::client::globals::registry_queue_init;
use sctk::reexports::client::Connection;
use sctk::registry::RegistryState;
use sctk::seat::keyboard::Modifiers;
use sctk::seat::SeatState;
use sctk::shell::wlr_layer::{Anchor, KeyboardInteractivity, Layer, LayerShell};
use sctk::shell::WaylandSurface;

use render::{EglContext, load_system_font};
use state::App;
use config::Config;
use glow::HasContext;

fn main() {
    env_logger::init();
    println!("Starting hypr-overlay-wl (EGL/GLES2)");

    let cfg = Config::load();
    Config::write_default_if_missing();

    let conn = Connection::connect_to_env().expect("Wayland connection failed");
    let (globals, event_queue) = registry_queue_init(&conn).expect("registry init failed");
    let qh = event_queue.handle();

    let compositor = CompositorState::bind(&globals, &qh).expect("wl_compositor not available");
    let layer_shell = LayerShell::bind(&globals, &qh).expect("layer shell not available");

    let surface = compositor.create_surface(&qh);
    let layer =
        layer_shell.create_layer_surface(&qh, surface, Layer::Overlay, Some("hypr_overlay"), None);

    layer.set_anchor(Anchor::TOP | Anchor::LEFT);
    layer.set_keyboard_interactivity(KeyboardInteractivity::None);
    layer.set_size(360, 64);
    layer.set_exclusive_zone(-1);
    layer.set_margin(cfg.initial_y, 0, 0, cfg.initial_x);
    layer.commit();

    let egl_ctx = EglContext::new(&conn, layer.wl_surface(), 360, 64);

    // Set up calloop event loop — integrates Wayland + Discord channel
    let mut event_loop: EventLoop<'static, App> = EventLoop::try_new().unwrap();
    let loop_handle = event_loop.handle();

    WaylandSource::new(conn, event_queue)
        .insert(loop_handle.clone())
        .unwrap();

    // Optional Discord IPC — enabled when both env vars are set
    let (discord_ev_tx, discord_ev_channel) = calloop::channel::channel::<discord::DiscordEvent>();
    loop_handle
        .insert_source(discord_ev_channel, |event, _, app| {
            if let calloop::channel::Event::Msg(ev) = event {
                if app.handle_discord_event(ev) {
                    app.draw();
                }
            }
        })
        .unwrap();

    // Combined animation + speaking-expiry tick
    loop_handle
        .insert_source(
            calloop::timer::Timer::from_duration(std::time::Duration::from_millis(16)),
            |_, _, app| {
                let now = std::time::Instant::now();
                let mut needs_redraw = false;

                // Animate participant rows (~180ms to fully appear/disappear)
                let anim_speed = 0.016_f32 / 0.18;
                let mut to_remove: Vec<String> = Vec::new();
                for p in &mut app.participants {
                    if p.leaving {
                        p.anim = (p.anim - anim_speed).max(0.0);
                        needs_redraw = true;
                        if p.anim <= 0.0 {
                            to_remove.push(p.user_id.clone());
                        }
                    } else if p.anim < 1.0 {
                        p.anim = (p.anim + anim_speed).min(1.0);
                        needs_redraw = true;
                    }
                }
                // Remove fully-animated-out participants and clean up textures
                for uid in &to_remove {
                    if let Some(pos) = app.participants.iter().position(|p| &p.user_id == uid) {
                        let name = app.participants[pos].display_name.clone();
                        eprintln!("[overlay] {name} removed from list");
                        app.participants.remove(pos);
                        if let Some((tex, _, _)) = app.name_textures.remove(uid) {
                            unsafe {
                                app.egl.gl.delete_texture(tex);
                            }
                        }
                        if let Some(tex) = app.avatar_textures.remove(uid) {
                            unsafe {
                                app.egl.gl.delete_texture(tex);
                            }
                        }
                    }
                }
                if !to_remove.is_empty() {
                    // Clamp scroll offset when participants are removed
                    let max_offset = app.participants.len().saturating_sub(app.max_visible_rows);
                    app.scroll_offset = app.scroll_offset.min(max_offset);
                    let extra = if app.participants.len() > app.max_visible_rows { 20 } else { 0 };
                    let new_h = 64 + app.visible_row_count() as u32 * 48 + extra;
                    app.resize_overlay(new_h);
                    needs_redraw = true;
                }

                // Expire speaking rings
                let any_expired = app
                    .participants
                    .iter()
                    .any(|p| p.speaking_until.map(|t| t <= now).unwrap_or(false));
                if any_expired {
                    for p in &mut app.participants {
                        if p.speaking_until.map(|t| t <= now).unwrap_or(false) {
                            p.speaking_until = None;
                        }
                    }
                    needs_redraw = true;
                }

                // Animate idle alpha (~250ms transition)
                // Target is 1.0 when in channel, 0.0 when not (fully hidden)
                let target_alpha = if app.in_channel { 1.0_f32 } else { 0.0_f32 };
                if (app.idle_alpha - target_alpha).abs() > 0.005 {
                    let speed = 0.016 / 0.25;
                    if app.idle_alpha < target_alpha {
                        app.idle_alpha = (app.idle_alpha + speed).min(target_alpha);
                    } else {
                        app.idle_alpha = (app.idle_alpha - speed).max(target_alpha);
                    }
                    needs_redraw = true;
                }

                // When fully hidden, clear the input region so no mouse events are consumed
                if !app.in_channel && app.idle_alpha <= 0.005 && app.idle_alpha > -0.001 {
                    app.clear_input_region();
                }

                // Update session timer texture every second
                if let Some(joined_at) = app.channel_joined_at {
                    let elapsed = joined_at.elapsed().as_secs() as u32;
                    if elapsed != app.last_timer_secs {
                        app.last_timer_secs = elapsed;
                        let h = elapsed / 3600;
                        let m = (elapsed % 3600) / 60;
                        let s = elapsed % 60;
                        let label = if h > 0 {
                            format!("{h}:{m:02}:{s:02}")
                        } else {
                            format!("{m}:{s:02}")
                        };
                        if let Some((tex, _, _)) = app.timer_tex.take() {
                            unsafe { app.egl.gl.delete_texture(tex); }
                        }
                        let new_tex = app.render_text_tex(&label, 12.0);
                        app.timer_tex = new_tex;
                        needs_redraw = true;
                    }
                }

                if needs_redraw {
                    app.draw();
                }

                // Run at 16ms when animating, 500ms when idle or just tracking timer
                let target_alpha = if app.in_channel { 1.0_f32 } else { 0.0_f32 };
                let animating = app.participants.iter().any(|p| p.anim < 1.0 || p.leaving)
                    || (app.idle_alpha - target_alpha).abs() > 0.005;
                let next = if animating {
                    std::time::Duration::from_millis(16)
                } else {
                    // 500ms is fine for the session timer (≤500ms display lag per second)
                    std::time::Duration::from_millis(500)
                };
                calloop::timer::TimeoutAction::ToDuration(next)
            },
        )
        .unwrap();

    let discord_cmd_tx = if !cfg.discord_client_id.is_empty() && !cfg.discord_client_secret.is_empty() {
        let (tx, rx) = mpsc::sync_channel(32);
        discord::spawn(
            discord::Config {
                client_id: cfg.discord_client_id.clone(),
                client_secret: cfg.discord_client_secret.clone(),
            },
            discord_ev_tx,
            rx,
        );
        println!("Discord IPC enabled — waiting for connection...");
        Some(tx)
    } else {
        println!(
            "Discord IPC disabled — set discord_client_id and discord_client_secret in\n  ~/.config/hypr-overlay/config.toml"
        );
        None
    };

    let mut app = App {
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        compositor,
        layer,
        egl: egl_ctx,
        width: 360,
        height: 64,
        dragging: false,
        last_pointer: (0, 0),
        drag_base_pos: (cfg.initial_x, cfg.initial_y),
        drag_output: None,
        margins: (cfg.initial_y, 0, 0, cfg.initial_x),
        anchor: Anchor::TOP | Anchor::LEFT,
        modifiers: Modifiers::default(),
        exit: false,
        discord_cmd_tx,
        discord_mute: false,
        discord_deaf: false,
        opacity: std::env::var("OVERLAY_OPACITY")
            .ok()
            .and_then(|s| s.parse::<f32>().ok())
            .unwrap_or(cfg.opacity)
            .clamp(0.1, 1.0),
        participants: vec![],
        avatar_textures: std::collections::HashMap::new(),
        name_textures: std::collections::HashMap::new(),
        font: load_system_font(),
        channel_name: None,
        channel_name_tex: None,
        guild_name: None,
        guild_name_tex: None,
        in_channel: false,
        idle_alpha: 0.0,
        channel_joined_at: None,
        timer_tex: None,
        last_timer_secs: u32::MAX,
        scroll_offset: 0,
        max_visible_rows: cfg.max_visible_rows,
        scroll_indicator_tex: None,
        last_scroll_state: (usize::MAX, usize::MAX),
        last_pointer_y: 0.0,
        config: cfg,
    };

    // Config hot-reload via inotify: watch config dir and send () on changes
    let (inotify_reload_tx, inotify_reload_rx) = calloop::channel::channel::<()>();
    {
        use inotify::{Inotify, WatchMask};
        let config_path = config::config_path();
        std::thread::spawn(move || {
            let mut inotify = match Inotify::init() {
                Ok(i) => i,
                Err(e) => { eprintln!("[config] inotify init failed: {e}"); return; }
            };
            if let Some(dir) = config_path.parent() {
                if let Err(e) = inotify.watches().add(dir, WatchMask::CLOSE_WRITE | WatchMask::MOVED_TO) {
                    eprintln!("[config] inotify watch add failed: {e}"); return;
                }
            }
            let config_filename = config_path.file_name().map(|f| f.to_owned());
            let mut buf = [0u8; 1024];
            loop {
                match inotify.read_events_blocking(&mut buf) {
                    Ok(events) => {
                        let changed = events.into_iter().any(|e| {
                            e.name.map(|n| Some(n) == config_filename.as_deref()).unwrap_or(false)
                        });
                        if changed {
                            let _ = inotify_reload_tx.send(());
                        }
                    }
                    Err(e) => { eprintln!("[config] inotify read error: {e}"); break; }
                }
            }
        });
    }
    loop_handle
        .insert_source(inotify_reload_rx, |event, _, app| {
            if let calloop::channel::Event::Msg(()) = event {
                eprintln!("[config] config file changed, reloading...");
                let new_cfg = Config::load();
                app.opacity = new_cfg.opacity;
                app.max_visible_rows = new_cfg.max_visible_rows;
                // Regenerate participant name textures with (possibly) new font size
                let user_ids: Vec<String> = app.participants.iter().map(|p| p.user_id.clone()).collect();
                let names: Vec<String> = app.participants.iter().map(|p| p.display_name.clone()).collect();
                app.config = new_cfg;
                for (uid, name) in user_ids.iter().zip(names.iter()) {
                    if let Some((tex, _, _)) = app.name_textures.remove(uid) {
                        unsafe { app.egl.gl.delete_texture(tex); }
                    }
                    app.make_name_texture(uid, name);
                }
                app.draw();
            }
        })
        .expect("inotify reload source");

    let loop_signal = event_loop.get_signal();
    event_loop
        .run(None, &mut app, move |app| {
            if app.exit {
                loop_signal.stop();
            }
        })
        .expect("event loop error");

    println!("hypr-overlay exiting");
}
