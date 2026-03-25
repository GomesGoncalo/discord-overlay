//! hypr-overlay-wl — phase 2: EGL/GLES2 hardware-accelerated layer-shell overlay.
//!
//! Transparent except for two rounded buttons (Mute/Deafen) and a drag handle.
//! Click-through everywhere else via wl_surface.set_input_region.
//! Discord IPC: set DISCORD_CLIENT_ID + DISCORD_CLIENT_SECRET to enable.

mod avatar;
mod config;
mod discord;
mod handlers;
mod latency;
mod render;
mod render_error;
mod state;

use std::sync::mpsc;

use calloop::EventLoop;
use calloop_wayland_source::WaylandSource;
use sctk::compositor::CompositorState;
use sctk::output::OutputState;
use sctk::reexports::client::globals::registry_queue_init;
use sctk::reexports::client::Connection;
use sctk::registry::RegistryState;
use sctk::seat::keyboard::Modifiers;
use sctk::seat::SeatState;
use sctk::shell::wlr_layer::{Anchor, KeyboardInteractivity, Layer, LayerShell};
use sctk::shell::WaylandSurface;
use smithay_client_toolkit as sctk;

use config::Config;
#[cfg(not(test))]
use render::EglContext;
use render::{load_system_font, EglBackend};
use state::App;
use tracing::{debug, error, info, trace, warn};

fn main() {
    use tracing_subscriber::{fmt, EnvFilter};
    fmt()
        .with_env_filter(
            EnvFilter::try_from_env("RUST_LOG")
                .unwrap_or_else(|_| EnvFilter::new("hypr_overlay_wl=info")),
        )
        .with_target(false)
        .with_thread_ids(false)
        .compact()
        .init();
    info!("Starting hypr-overlay-wl (EGL/GLES2)");

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

    let egl_ctx: Box<dyn EglBackend> = {
        #[cfg(not(test))]
        {
            Box::new(EglContext::new(&conn, layer.wl_surface(), 360, 64))
        }
        #[cfg(test)]
        {
            Box::new(crate::render::MockEgl::new())
        }
    };

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
                debug!("EVENT: Discord event received: {:?}", ev);
                if app.handle_discord_event(ev) {
                    debug!("EVENT: handle_discord_event returned true, calling draw()");
                    app.draw();
                } else {
                    debug!("EVENT: handle_discord_event returned false, skipping draw()");
                }
            }
        })
        .unwrap();

    // Combined animation + speaking-expiry tick (~180ms join/leave, 250ms idle fade)
    loop_handle
        .insert_source(
            calloop::timer::Timer::from_duration(std::time::Duration::from_millis(16)),
            |_, _, app| {
                let now = std::time::Instant::now();
                // anim_speed: full travel in ~180ms at a 16ms tick rate
                const ANIM_SPEED: f32 = 0.016 / 0.18;

                let mut needs_redraw = false;
                needs_redraw |= app.tick_animations(ANIM_SPEED);
                needs_redraw |= app.tick_speaking_expiry(now);
                needs_redraw |= app.tick_ptt(now);
                needs_redraw |= app.tick_idle_alpha();
                needs_redraw |= app.tick_timer();
                needs_redraw |= app.tick_talk_time_textures();

                if needs_redraw {
                    trace!("timer tick: redrawing");
                    app.draw();
                } else {
                    trace!("timer tick: no redraw needed");
                }

                // Run at 16ms when animating, 500ms when idle or just tracking timer
                let target_alpha = if app.in_channel { 1.0_f32 } else { 0.0_f32 };
                let animating = app.participants.iter().any(|p| {
                    p.anim < 1.0 || p.leaving || p.speaking_anim > 0.005 && p.speaking_anim < 0.995
                }) || app.participants.iter().any(|p| p.speaking_anim > 0.5)
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

    let discord_cmd_tx = if !cfg.discord_client_id.is_empty()
        && !cfg.discord_client_secret.is_empty()
    {
        let (tx, rx) = mpsc::sync_channel(32);
        discord::spawn(
            discord::Config {
                client_id: cfg.discord_client_id.clone(),
                client_secret: cfg.discord_client_secret.clone(),
            },
            discord_ev_tx.clone(),
            rx,
        );
        info!("Discord IPC enabled — waiting for connection...");
        Some(tx)
    } else {
        error!(
            "Discord IPC disabled — set discord_client_id and discord_client_secret in\n  ~/.config/hypr-overlay/config.toml"
        );
        None
    };

    // Background latency probe — measures TCP RTT to discord.com:443 every 10s
    {
        use latency::{LatencyProbe, TcpPing};
        let ping_tx = discord_ev_tx.clone();
        std::thread::spawn(move || {
            let probe = TcpPing::new("discord.com:443");
            loop {
                if let Some(ms) = probe.measure() {
                    let _ = ping_tx.send(discord::DiscordEvent::PingResult { latency_ms: ms });
                }
                std::thread::sleep(std::time::Duration::from_secs(10));
            }
        });
    }

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
        initials_textures: std::collections::HashMap::new(),
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
        compact: cfg.compact_by_default,
        last_click_time: None,
        ptt_mode: false,
        ptt_active: false,
        self_user_id: crate::discord::UserId::default(),
        participant_count_tex: None,
        last_participant_count: usize::MAX,
        overflow_badge_tex: None,
        last_overflow_count: usize::MAX,
        ellipsis_tex: None,
        speaking_pulse_phase: 0.0,
        mute_held: false,
        deaf_held: false,
        talk_time_textures: std::collections::HashMap::new(),
        last_talk_time_secs: std::collections::HashMap::new(),
        last_idle_label: std::collections::HashMap::new(),
        ping_ms: None,
        ping_tex: None,
        ping_samples: std::collections::VecDeque::new(),
        jitter_tex: None,
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
                Err(e) => {
                    warn!("inotify init failed: {e}");
                    return;
                }
            };
            if let Some(dir) = config_path.parent() {
                if let Err(e) = inotify
                    .watches()
                    .add(dir, WatchMask::CLOSE_WRITE | WatchMask::MOVED_TO)
                {
                    warn!("inotify watch add failed: {e}");
                    return;
                }
            }
            let config_filename = config_path.file_name().map(|f| f.to_owned());
            let mut buf = [0u8; 1024];
            loop {
                match inotify.read_events_blocking(&mut buf) {
                    Ok(events) => {
                        let changed = events.into_iter().any(|e| {
                            e.name
                                .map(|n| Some(n) == config_filename.as_deref())
                                .unwrap_or(false)
                        });
                        if changed {
                            let _ = inotify_reload_tx.send(());
                        }
                    }
                    Err(e) => {
                        warn!("inotify read error: {e}");
                        break;
                    }
                }
            }
        });
    }
    loop_handle
        .insert_source(inotify_reload_rx, |event, _, app| {
            if let calloop::channel::Event::Msg(()) = event {
                info!("config file changed, reloading...");
                let new_cfg = Config::load();
                app.opacity = new_cfg.opacity;
                app.max_visible_rows = new_cfg.max_visible_rows;
                // Collect data needed for texture regeneration before updating config
                let participants: Vec<(crate::discord::UserId, String)> = app
                    .participants
                    .iter()
                    .map(|p| (p.user_id.clone(), p.display_name.clone()))
                    .collect();
                let channel_name = app.channel_name.clone();
                let guild_name = app.guild_name.clone();
                app.config = new_cfg;
                // Regenerate name + initials textures with new font size
                for (uid, name) in &participants {
                    if let Some((tex, _, _)) = app.name_textures.remove(uid) {
                        app.egl.delete_texture(tex);
                    }
                    app.make_name_texture(uid, name);
                    if let Some((tex, _, _)) = app.initials_textures.remove(uid) {
                        app.egl.delete_texture(tex);
                    }
                    app.ensure_initial_texture(uid, name);
                }
                // Regenerate channel name texture
                if let Some((tex, _, _)) = app.channel_name_tex.take() {
                    app.egl.delete_texture(tex);
                }
                if let Some(ref name) = channel_name {
                    let display = format!("# {name}");
                    app.channel_name_tex =
                        app.render_text_tex(&display, app.config.font_size * 0.93);
                }
                // Regenerate guild name texture
                if let Some((tex, _, _)) = app.guild_name_tex.take() {
                    app.egl.delete_texture(tex);
                }
                if let Some(ref name) = guild_name {
                    app.guild_name_tex = app.render_text_tex(name, app.config.font_size * 0.79);
                }
                // Reset sentinels so timer, count, scroll indicator regenerate on next draw/tick
                if let Some((tex, _, _)) = app.timer_tex.take() {
                    app.egl.delete_texture(tex);
                }
                app.last_timer_secs = u32::MAX;
                app.last_participant_count = usize::MAX;
                app.last_scroll_state = (usize::MAX, usize::MAX);
                // Invalidate ellipsis and overflow badge (font-size-dependent)
                if let Some((tex, _, _)) = app.ellipsis_tex.take() {
                    app.egl.delete_texture(tex);
                }
                if let Some((tex, _, _)) = app.overflow_badge_tex.take() {
                    app.egl.delete_texture(tex);
                }
                app.last_overflow_count = usize::MAX;
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

    info!("hypr-overlay exiting");
}
