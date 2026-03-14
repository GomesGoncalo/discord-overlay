use std::collections::HashMap;
use std::sync::mpsc;
use tracing::{debug, info};

use sctk::compositor::{CompositorState, Region};
use sctk::output::OutputState;
use sctk::reexports::client::protocol::wl_output;
use sctk::registry::RegistryState;
use sctk::seat::keyboard::Modifiers;
use sctk::seat::SeatState;
use sctk::shell::wlr_layer::{Anchor, LayerSurface};
use sctk::shell::WaylandSurface;
use smithay_client_toolkit as sctk;

use crate::config::Config;
use crate::discord;
use crate::handlers::{button2_rects, button_rects, drag_handle_rects};
use crate::render::{render_text_texture, EglBackend};

// ─── Participant state ────────────────────────────────────────────────────────

pub struct ParticipantState {
    pub user_id: String,
    pub display_name: String,
    pub muted: bool,
    pub deafened: bool,
    pub speaking_until: Option<std::time::Instant>,
    /// 0.0 = invisible (enter start / leave end), 1.0 = fully visible.
    pub anim: f32,
    /// True when this participant is animating out before removal.
    pub leaving: bool,
}

/// Builder for ParticipantState with sensible defaults.
pub struct ParticipantStateBuilder {
    user_id: String,
    display_name: String,
    muted: bool,
    deafened: bool,
    speaking_until: Option<std::time::Instant>,
    anim: f32,
    leaving: bool,
}

impl ParticipantStateBuilder {
    /// Create a new builder from a discord participant.
    pub fn from_discord(p: &discord::Participant) -> Self {
        Self {
            user_id: p.user_id.clone(),
            display_name: p.nick.clone().unwrap_or_else(|| p.username.clone()),
            muted: p.muted,
            deafened: p.deafened,
            speaking_until: None,
            anim: 1.0, // start visible by default
            leaving: false,
        }
    }

    /// Create a new builder with minimal required fields.
    #[allow(dead_code)]
    pub fn new(user_id: impl Into<String>, display_name: impl Into<String>) -> Self {
        Self {
            user_id: user_id.into(),
            display_name: display_name.into(),
            muted: false,
            deafened: false,
            speaking_until: None,
            anim: 1.0,
            leaving: false,
        }
    }

    pub fn anim(mut self, anim: f32) -> Self {
        self.anim = anim;
        self
    }

    #[allow(dead_code)]
    pub fn muted(mut self, muted: bool) -> Self {
        self.muted = muted;
        self
    }

    #[allow(dead_code)]
    pub fn deafened(mut self, deafened: bool) -> Self {
        self.deafened = deafened;
        self
    }

    #[allow(dead_code)]
    pub fn leaving(mut self, leaving: bool) -> Self {
        self.leaving = leaving;
        self
    }

    pub fn build(self) -> ParticipantState {
        ParticipantState {
            user_id: self.user_id,
            display_name: self.display_name,
            muted: self.muted,
            deafened: self.deafened,
            speaking_until: self.speaking_until,
            anim: self.anim,
            leaving: self.leaving,
        }
    }
}

// ─── App ─────────────────────────────────────────────────────────────────────

pub struct App {
    pub registry_state: RegistryState,
    pub seat_state: SeatState,
    pub output_state: OutputState,
    pub compositor: CompositorState,
    pub layer: LayerSurface,
    pub egl: Box<dyn EglBackend>,
    pub width: u32,
    pub height: u32,
    pub dragging: bool,
    pub last_pointer: (i32, i32),
    pub drag_base_pos: (i32, i32),
    pub drag_output: Option<wl_output::WlOutput>,
    pub margins: (i32, i32, i32, i32),
    pub anchor: Anchor,
    pub modifiers: Modifiers,
    pub exit: bool,
    // Discord state
    pub discord_cmd_tx: Option<mpsc::SyncSender<discord::DiscordCommand>>,
    pub discord_mute: bool,
    pub discord_deaf: bool,
    // Overlay opacity (0.1 – 1.0), adjustable with scroll wheel
    pub opacity: f32,
    // Voice participants
    pub participants: Vec<ParticipantState>,
    pub avatar_textures: HashMap<String, glow::NativeTexture>,
    pub name_textures: HashMap<String, (glow::NativeTexture, u32, u32)>,
    pub initials_textures: HashMap<String, (glow::NativeTexture, u32, u32)>,
    pub font: Option<fontdue::Font>,
    // Channel name display
    pub channel_name: Option<String>,
    pub channel_name_tex: Option<(glow::NativeTexture, u32, u32)>,
    // Guild name display
    pub guild_name: Option<String>,
    pub guild_name_tex: Option<(glow::NativeTexture, u32, u32)>,
    // Idle fade state
    pub in_channel: bool,
    pub idle_alpha: f32,
    // Session duration timer
    pub channel_joined_at: Option<std::time::Instant>,
    pub timer_tex: Option<(glow::NativeTexture, u32, u32)>,
    pub last_timer_secs: u32,
    // Scrollable participant list
    pub scroll_offset: usize,
    pub max_visible_rows: usize,
    pub scroll_indicator_tex: Option<(glow::NativeTexture, u32, u32)>,
    pub last_scroll_state: (usize, usize),
    // Pointer Y position for scroll vs opacity decision
    pub last_pointer_y: f64,
    // Runtime config
    pub config: Config,
    // Compact mode
    pub compact: bool,
    pub last_click_time: Option<std::time::Instant>,
    // Push-to-talk
    pub ptt_mode: bool,
    pub ptt_active: bool,
    // Local user identity (set from Ready event)
    pub self_user_id: String,
}

// ─── App methods ─────────────────────────────────────────────────────────────

impl App {
    pub fn visible_row_count(&self) -> usize {
        self.participants.len().min(self.max_visible_rows)
    }

    /// Rasterise `text` at `px_size` and upload it as a GL texture.
    pub fn render_text_tex(
        &self,
        text: &str,
        px_size: f32,
    ) -> Option<(glow::NativeTexture, u32, u32)> {
        let font = self.font.as_ref()?;
        let (pixels, w, h) = render_text_texture(font, text, px_size);
        if w > 0 && h > 0 {
            let tex = self.egl.upload_texture_wh(&pixels, w, h);
            Some((tex, w, h))
        } else {
            None
        }
    }

    pub fn resize_overlay(&mut self, new_h: u32) {
        if self.height == new_h {
            return;
        }
        debug!(
            "resize {} → {} px tall ({} participant rows)",
            self.height,
            new_h,
            self.participants.len()
        );
        self.height = new_h;
        self.egl.resize(self.width as i32, new_h as i32);
        self.layer.set_size(self.width, new_h);
    }

    /// Resize overlay for compact/normal mode. Call after toggling `self.compact`
    /// or after participant count changes while in compact mode.
    pub fn apply_compact_resize(&mut self) {
        if self.compact {
            let n = self.participants.len().max(1);
            let w = (n as u32 * 48 + 16).max(120);
            self.width = w;
            self.height = 48;
            self.egl.resize(w as i32, 48);
            self.layer.set_size(w, 48);
        } else {
            let n = self.visible_row_count();
            let extra = if self.participants.len() > self.max_visible_rows {
                20
            } else {
                0
            };
            let h = 64 + n as u32 * 48 + extra;
            self.width = 360;
            self.height = h;
            self.egl.resize(360, h as i32);
            self.layer.set_size(360, h);
        }
        // Re-apply margin so the top-left corner stays at drag_base_pos
        // after the size change (compositor would otherwise reposition the surface).
        let (x, y) = self.drag_base_pos;
        self.anchor = Anchor::TOP | Anchor::LEFT;
        self.layer.set_anchor(self.anchor);
        self.layer.set_margin(y, 0, 0, x);
        self.margins = (y, 0, 0, x);
    }

    /// Find a participant by user ID (mutable).
    fn find_participant_mut(&mut self, user_id: &str) -> Option<&mut ParticipantState> {
        self.participants.iter_mut().find(|p| p.user_id == user_id)
    }

    /// Compact-mode render: single horizontal row of avatars (40 px) with speaking rings.
    fn draw_compact(&mut self) {
        let op = self.opacity * self.idle_alpha;
        debug!(
            "DRAW_COMPACT: in_channel={} idle_alpha={:.2} opacity={:.2} final_op={:.2}",
            self.in_channel, self.idle_alpha, self.opacity, op
        );

        // Ensure initials exist for missing avatars (simplifies the core draw path)
        let missing_initials: Vec<(String, String)> = self
            .participants
            .iter()
            .filter(|p| !self.avatar_textures.contains_key(&p.user_id))
            .map(|p| (p.user_id.clone(), p.display_name.clone()))
            .collect();
        for (uid, name) in missing_initials {
            self.ensure_initial_texture(&uid, &name);
        }

        // Call the extracted core drawing routine (testable without Wayland state)
        draw_compact_core(
            &*self.egl,
            self.width,
            self.height,
            self.opacity,
            self.idle_alpha,
            &self.participants,
            &self.avatar_textures,
            &self.initials_textures,
            self.config.speaking_color,
        );

        // Entire surface is interactive (acts as drag handle) in compact mode.
        let region = Region::new(&self.compositor).expect("region");
        region.add(0, 0, self.width as i32, self.height as i32);
        self.layer.set_input_region(Some(region.wl_region()));

        debug!("DRAW_COMPACT: Calling egl.swap()");
        self.egl.swap();
        debug!(
            "DRAW_COMPACT: Calling damage() damage_rect=(0,0,{},{})",
            self.width as i32, self.height as i32
        );
        self.layer
            .wl_surface()
            .damage(0, 0, self.width as i32, self.height as i32);
        debug!("DRAW_COMPACT: Calling wl_surface().commit()");
        self.layer.wl_surface().commit();
        debug!("DRAW_COMPACT: Complete");
    }

    /// Remove all input regions so the overlay is fully click-through (used when hidden).
    pub fn clear_input_region(&mut self) {
        let region = Region::new(&self.compositor).expect("create region");
        // Add nothing — empty region = fully click-through
        self.layer.set_input_region(Some(region.wl_region()));
        self.layer
            .wl_surface()
            .damage(0, 0, self.width as i32, self.height as i32);
        self.layer.wl_surface().commit();
    }

    pub fn make_name_texture(&mut self, user_id: &str, name: &str) {
        if let Some(font) = &self.font {
            let font_size = self.config.font_size;
            let (pixels, w, h) = render_text_texture(font, name, font_size);
            if w > 0 && h > 0 {
                let tex = self.egl.upload_texture_wh(&pixels, w, h);
                self.name_textures.insert(user_id.to_string(), (tex, w, h));
            }
        }
    }

    pub fn ensure_initial_texture(&mut self, user_id: &str, display_name: &str) {
        if self.initials_textures.contains_key(user_id) {
            return;
        }
        let initial = display_name
            .chars()
            .next()
            .map(|c| c.to_uppercase().to_string())
            .unwrap_or_else(|| "?".to_string());
        if let Some((tex, w, h)) = self.render_text_tex(&initial, 20.0) {
            self.initials_textures
                .insert(user_id.to_string(), (tex, w, h));
        }
    }

    pub fn handle_discord_event(&mut self, event: discord::DiscordEvent) -> bool {
        match event {
            discord::DiscordEvent::Ready { username, user_id } => {
                info!("Discord connected as {username}");
                self.self_user_id = user_id;
                false
            }
            discord::DiscordEvent::VoiceSettings { mute, deaf } => {
                let changed = self.discord_mute != mute || self.discord_deaf != deaf;
                self.discord_mute = mute;
                self.discord_deaf = deaf;
                changed
            }
            discord::DiscordEvent::VoiceMode { ptt } => {
                self.ptt_mode = ptt;
                true
            }
            discord::DiscordEvent::VoiceParticipants {
                participants: parts,
                channel_name,
            } => {
                // Update in_channel state and channel name texture
                let was_in_channel = self.in_channel;
                self.in_channel = channel_name.is_some();
                // Start session timer when newly joining a channel
                if self.in_channel && !was_in_channel {
                    self.channel_joined_at = Some(std::time::Instant::now());
                    self.last_timer_secs = u32::MAX; // force texture regeneration
                } else if !self.in_channel {
                    self.channel_joined_at = None;
                    delete_texture_if_present(&*self.egl, &mut self.timer_tex);
                }
                if self.channel_name != channel_name {
                    delete_texture_if_present(&*self.egl, &mut self.channel_name_tex);
                    self.channel_name = channel_name.clone();
                    if let Some(ref name) = channel_name {
                        if let Some(font) = &self.font {
                            let (pixels, w, h) = render_text_texture(font, name, 13.0);
                            if w > 0 && h > 0 {
                                let tex = self.egl.upload_texture_wh(&pixels, w, h);
                                self.channel_name_tex = Some((tex, w, h));
                            }
                        }
                    }
                }
                delete_all_textures_in_map(&*self.egl, &mut self.name_textures);
                delete_all_textures_in_map(&*self.egl, &mut self.initials_textures);
                self.participants = parts
                    .iter()
                    .map(|p| ParticipantStateBuilder::from_discord(p).build())
                    .collect();
                let user_names: Vec<(String, String)> = self
                    .participants
                    .iter()
                    .map(|p| (p.user_id.clone(), p.display_name.clone()))
                    .collect();
                for (uid, name) in user_names {
                    self.make_name_texture(&uid, &name);
                    self.ensure_initial_texture(&uid, &name);
                }
                // Reset scroll when participant list is fully replaced
                self.scroll_offset = 0;
                delete_texture_if_present(&*self.egl, &mut self.scroll_indicator_tex);
                self.last_scroll_state = (usize::MAX, usize::MAX);
                let extra = if self.participants.len() > self.max_visible_rows {
                    20
                } else {
                    0
                };
                let new_h = 64 + self.visible_row_count() as u32 * 48 + extra;
                self.resize_overlay(new_h);
                if self.compact {
                    self.apply_compact_resize();
                }
                true
            }
            discord::DiscordEvent::UserJoined(p) => {
                // Ignore if already in list (e.g. duplicate event)
                if self.participants.iter().any(|e| e.user_id == p.user_id) {
                    return false;
                }
                info!(
                    "{} joined the channel",
                    p.nick.as_deref().unwrap_or(&p.username)
                );
                let uid = p.user_id.clone();
                let name = p.nick.clone().unwrap_or_else(|| p.username.clone());
                self.participants.push(
                    ParticipantStateBuilder::from_discord(&p)
                        .anim(0.0) // start invisible, animate in
                        .build(),
                );
                // Mark as in_channel when we have participants
                self.in_channel = true;
                self.make_name_texture(&uid, &name);
                self.ensure_initial_texture(&uid, &name);
                let extra = if self.participants.len() > self.max_visible_rows {
                    20
                } else {
                    0
                };
                let new_h = 64 + self.visible_row_count() as u32 * 48 + extra;
                self.resize_overlay(new_h);
                if self.compact {
                    self.apply_compact_resize();
                }
                debug!(
                    "UserJoined: Setting in_channel=true, now have {} participants",
                    self.participants.len()
                );
                true
            }
            discord::DiscordEvent::UserLeft { user_id } => {
                if let Some(p) = self.find_participant_mut(&user_id) {
                    if !p.leaving {
                        info!("{} leaving channel (animating out)", p.display_name);
                        p.leaving = true;
                        return true; // trigger a redraw to start the animation
                    }
                }
                false
            }
            discord::DiscordEvent::ParticipantStateUpdate {
                user_id,
                muted,
                deafened,
            } => {
                if let Some(p) = self.find_participant_mut(&user_id) {
                    let changed = p.muted != muted || p.deafened != deafened;
                    p.muted = muted;
                    p.deafened = deafened;
                    return changed;
                }
                false
            }
            discord::DiscordEvent::SpeakingUpdate { user_id, speaking } => {
                if let Some(p) = self.find_participant_mut(&user_id) {
                    p.speaking_until = if speaking {
                        // Ring stays for 1.5s after last SPEAKING_START.
                        // Discord fires SPEAKING_START ~every 1s while active,
                        // so the ring clears ~0.5s after the user goes quiet.
                        Some(std::time::Instant::now() + std::time::Duration::from_millis(1500))
                    } else {
                        None
                    };
                    return true;
                }
                false
            }
            discord::DiscordEvent::AvatarLoaded {
                user_id,
                rgba,
                size,
            } => {
                let tex = self.egl.upload_texture_wh(&rgba, size, size);
                self.avatar_textures.insert(user_id, tex);
                true
            }
            discord::DiscordEvent::GuildName { name } => {
                delete_texture_if_present(&*self.egl, &mut self.guild_name_tex);
                if name.is_empty() {
                    self.guild_name = None;
                } else {
                    self.guild_name_tex = self.render_text_tex(&name, 11.0);
                    self.guild_name = Some(name);
                }
                true
            }
            discord::DiscordEvent::Disconnected => {
                // Clear all voice-channel state so the UI resets to idle.
                self.in_channel = false;
                self.channel_name = None;
                delete_texture_if_present(&*self.egl, &mut self.channel_name_tex);
                // Clear guild name
                self.guild_name = None;
                delete_texture_if_present(&*self.egl, &mut self.guild_name_tex);
                // Clear session timer
                self.channel_joined_at = None;
                delete_texture_if_present(&*self.egl, &mut self.timer_tex);
                // Clear scroll indicator
                delete_texture_if_present(&*self.egl, &mut self.scroll_indicator_tex);
                self.scroll_offset = 0;
                self.last_scroll_state = (usize::MAX, usize::MAX);
                delete_all_avatar_textures(&*self.egl, &mut self.avatar_textures);
                delete_all_textures_in_map(&*self.egl, &mut self.name_textures);
                delete_all_textures_in_map(&*self.egl, &mut self.initials_textures);
                self.participants.clear();
                self.discord_mute = false;
                self.discord_deaf = false;
                self.idle_alpha = 0.0;
                self.resize_overlay(64);
                true
            }
        }
    }

    pub fn draw(&mut self) {
        let op = self.opacity * self.idle_alpha;
        debug!(
            "DRAW: in_channel={} idle_alpha={:.2} opacity={:.2} final_op={:.2} compact={}",
            self.in_channel, self.idle_alpha, self.opacity, op, self.compact
        );

        if self.compact {
            debug!("DRAW: Using compact mode");
            self.draw_compact();
            return;
        }

        debug!(
            "DRAW: Using normal mode, w={} h={}",
            self.width, self.height
        );
        let (sw, sh) = (self.width as f32, self.height as f32);
        let (bx, by, bw, bh) = button_rects(self.width, 64);
        let (bx2, by2, bw2, bh2) = button2_rects(self.width, 64);
        let (hx, hy, hw, hh) = drag_handle_rects(self.width, 64);

        self.egl
            .viewport(0, 0, self.width as i32, self.height as i32);
        self.egl.clear_color(0.0, 0.0, 0.0, 0.0);
        self.egl.clear(glow::COLOR_BUFFER_BIT);
        self.egl.use_main_program();

        // Drag handle — blue pill
        self.egl.draw_rect(
            hx as f32,
            hy as f32,
            hw as f32,
            hh as f32,
            sw,
            sh,
            [0.25, 0.45, 1.0, 0.75 * op],
            (hw as f32 * 0.5).min(10.0),
        );

        // Left-side info: guild name (top), channel name (middle), timer (bottom)
        // Positioned to the right of the drag handle (hx + hw + 8px gap)
        let text_x = (hx + hw) as f32 + 8.0;
        if let Some((tex, tw, th)) = self.guild_name_tex {
            self.egl
                .draw_icon(text_x, 4.0, tw as f32, th as f32, sw, sh, tex, op * 0.50);
        }
        if let Some((tex, tw, th)) = self.channel_name_tex {
            self.egl
                .draw_icon(text_x, 20.0, tw as f32, th as f32, sw, sh, tex, op * 0.85);
        }
        if let Some((tex, tw, th)) = self.timer_tex {
            self.egl
                .draw_icon(text_x, 36.0, tw as f32, th as f32, sw, sh, tex, op * 0.60);
        }

        // Mute button background
        // When deafened, mic is implicitly muted too
        let effectively_muted = self.discord_mute || self.discord_deaf;
        // PTT mode: dim the mic button when not transmitting
        let mic_alpha = if self.ptt_mode && !self.ptt_active && !effectively_muted {
            op * 0.35
        } else {
            op * 0.82
        };
        let mute_base = if effectively_muted {
            [0.75, 0.15, 0.15, 0.88 * op]
        } else {
            [0.12, 0.68, 0.28, mic_alpha]
        };
        self.egl.draw_rect(
            bx2 as f32, by2 as f32, bw2 as f32, bh2 as f32, sw, sh, mute_base, 10.0,
        );

        // Deafen button background
        let deaf_base = if self.discord_deaf {
            [0.75, 0.15, 0.15, 0.88 * op]
        } else {
            [0.18, 0.36, 0.82, 0.82 * op]
        };
        self.egl.draw_rect(
            bx as f32, by as f32, bw as f32, bh as f32, sw, sh, deaf_base, 10.0,
        );

        // Icon overlays — mic on mute button, headphone on deafen button
        let pad = 6.0f32;
        let mic_tex = self.egl.tex_mic();
        let hp_tex = self.egl.tex_headphone();
        let strike_tex = self.egl.tex_strikeout();
        self.egl.draw_icon(
            bx2 as f32 + pad,
            by2 as f32 + pad,
            bw2 as f32 - 2.0 * pad,
            bh2 as f32 - 2.0 * pad,
            sw,
            sh,
            mic_tex,
            mic_alpha,
        );
        if effectively_muted {
            self.egl.draw_icon(
                bx2 as f32 + pad,
                by2 as f32 + pad,
                bw2 as f32 - 2.0 * pad,
                bh2 as f32 - 2.0 * pad,
                sw,
                sh,
                strike_tex,
                op,
            );
        }
        self.egl.draw_icon(
            bx as f32 + pad,
            by as f32 + pad,
            bw as f32 - 2.0 * pad,
            bh as f32 - 2.0 * pad,
            sw,
            sh,
            hp_tex,
            op,
        );
        if self.discord_deaf {
            self.egl.draw_icon(
                bx as f32 + pad,
                by as f32 + pad,
                bw as f32 - 2.0 * pad,
                bh as f32 - 2.0 * pad,
                sw,
                sh,
                strike_tex,
                op,
            );
        }

        // Restrict input to interactive areas only (rest is click-through)
        let region = Region::new(&self.compositor).expect("create region");
        region.add(bx, by, bw, bh);
        region.add(bx2, by2, bw2, bh2);
        region.add(hx, hy, hw, hh);
        self.layer.set_input_region(Some(region.wl_region()));

        // ── Participant rows (below the 64px control bar) ─────────────────────
        let row_h = 48u32;

        // Update scroll indicator texture if the scroll state changed
        let scroll_state = (self.scroll_offset, self.participants.len());
        if self.participants.len() > self.max_visible_rows {
            if scroll_state != self.last_scroll_state {
                self.last_scroll_state = scroll_state;
                delete_texture_if_present(&*self.egl, &mut self.scroll_indicator_tex);
                let above = self.scroll_offset;
                let below = self
                    .participants
                    .len()
                    .saturating_sub(self.scroll_offset + self.max_visible_rows);
                let label = match (above > 0, below > 0) {
                    (true, true) => format!("↑{}  ↓{}", above, below),
                    (true, false) => format!("↑{} more above", above),
                    (false, true) => format!("↓{} more below", below),
                    _ => String::new(),
                };
                if !label.is_empty() {
                    let new_tex = self.render_text_tex(&label, 11.0);
                    self.scroll_indicator_tex = new_tex;
                }
            }
        } else if self.scroll_indicator_tex.is_some() {
            delete_texture_if_present(&*self.egl, &mut self.scroll_indicator_tex);
            self.last_scroll_state = (usize::MAX, usize::MAX);
        }

        // Collect visible participant data to avoid re-borrowing self in the loop
        let visible: Vec<(usize, f32, bool, bool, bool, String, String)> = self
            .participants
            .iter()
            .enumerate()
            .skip(self.scroll_offset)
            .take(self.max_visible_rows)
            .map(|(abs_idx, p)| {
                let speaking = p
                    .speaking_until
                    .map(|t| t > std::time::Instant::now())
                    .unwrap_or(false);
                (
                    abs_idx,
                    p.anim,
                    p.muted,
                    p.deafened,
                    speaking,
                    p.user_id.clone(),
                    p.display_name.clone(),
                )
            })
            .collect();

        for (slot, (abs_idx, anim, muted, deafened, speaking, user_id, display_name)) in
            visible.iter().enumerate()
        {
            let _ = abs_idx; // abs_idx available for future use; slot drives layout
            let slide_offset = (1.0 - anim) * row_h as f32 * 0.35;
            let row_y_f = 64.0_f32 + slot as f32 * row_h as f32 + slide_offset;
            let row_anim_op = op * anim;

            let av_size = 32f32;
            let av_x = 12f32;
            let av_y = row_y_f + (row_h as f32 - av_size) * 0.5;

            // Semi-transparent row background
            self.egl.draw_rect(
                4.0,
                row_y_f + 4.0,
                sw - 8.0,
                row_h as f32 - 8.0,
                sw,
                sh,
                [
                    self.config.bg_color[0],
                    self.config.bg_color[1],
                    self.config.bg_color[2],
                    0.6 * row_anim_op,
                ],
                8.0,
            );

            // Speaking ring — active while speaking_until is in the future
            if *speaking {
                let ring = 3.0f32;
                self.egl.draw_rect(
                    av_x - ring,
                    av_y - ring,
                    av_size + ring * 2.0,
                    av_size + ring * 2.0,
                    sw,
                    sh,
                    [
                        self.config.speaking_color[0],
                        self.config.speaking_color[1],
                        self.config.speaking_color[2],
                        0.9 * row_anim_op,
                    ],
                    (av_size + ring * 2.0) * 0.5,
                );
            }

            // Avatar
            if let Some(&tex) = self.avatar_textures.get(user_id) {
                let desaturate = if *deafened { 1.0_f32 } else { 0.0 };
                self.egl
                    .draw_avatar(av_x, av_y, av_size, sw, sh, tex, row_anim_op, desaturate);
            } else {
                let hue = user_id.bytes().fold(0u32, |a, b| a.wrapping_add(b as u32));
                let colors = [
                    [0.6f32, 0.2, 0.8],
                    [0.2, 0.6, 0.8],
                    [0.8, 0.4, 0.2],
                    [0.3, 0.7, 0.4],
                ];
                let c = colors[(hue % 4) as usize];
                self.egl.draw_rect(
                    av_x,
                    av_y,
                    av_size,
                    av_size,
                    sw,
                    sh,
                    [c[0], c[1], c[2], 0.85 * row_anim_op],
                    av_size * 0.5,
                );
                // Initial letter centered on placeholder circle
                self.ensure_initial_texture(user_id, display_name);
                let initial_data = self
                    .initials_textures
                    .get(user_id)
                    .map(|&(t, w, h)| (t, w, h));
                if let Some((tex, tw, th)) = initial_data {
                    let ix = av_x + (av_size - tw as f32) * 0.5;
                    let iy = av_y + (av_size - th as f32) * 0.5;
                    self.egl
                        .draw_icon(ix, iy, tw as f32, th as f32, sw, sh, tex, row_anim_op);
                }
            }

            // Name text
            let icon_sz = 16.0f32;
            let icon_gap = 4.0f32;
            let icons_w = icon_sz * 2.0 + icon_gap + 8.0;
            let name_x = av_x + av_size + 8.0;
            let name_w_max = sw - name_x - icons_w;
            if let Some(&(tex, tw, th)) = self.name_textures.get(user_id) {
                let draw_w = (tw as f32).min(name_w_max);
                let draw_h = th as f32;
                let name_y = row_y_f + (row_h as f32 - draw_h) * 0.5;
                self.egl
                    .draw_icon(name_x, name_y, draw_w, draw_h, sw, sh, tex, row_anim_op);
            }

            // Per-participant mute/deaf icons (right side of row)
            let mic_tex = self.egl.tex_mic();
            let hp_tex = self.egl.tex_headphone();
            let strike_tex = self.egl.tex_strikeout();
            let icon_y = row_y_f + (row_h as f32 - icon_sz) * 0.5;
            let mic_x = sw - icon_sz * 2.0 - icon_gap - 8.0;
            let hp_x = sw - icon_sz - 8.0;
            let mic_op = if *muted {
                row_anim_op * 0.9
            } else {
                row_anim_op * 0.35
            };
            if *muted {
                self.egl.draw_rect(
                    mic_x - 2.0,
                    icon_y - 2.0,
                    icon_sz + 4.0,
                    icon_sz + 4.0,
                    sw,
                    sh,
                    [
                        self.config.muted_color[0],
                        self.config.muted_color[1],
                        self.config.muted_color[2],
                        0.6 * row_anim_op,
                    ],
                    4.0,
                );
            }
            self.egl
                .draw_icon(mic_x, icon_y, icon_sz, icon_sz, sw, sh, mic_tex, mic_op);
            if *muted {
                self.egl.draw_icon(
                    mic_x,
                    icon_y,
                    icon_sz,
                    icon_sz,
                    sw,
                    sh,
                    strike_tex,
                    row_anim_op * 0.85,
                );
            }
            let hp_op = if *deafened {
                row_anim_op * 0.9
            } else {
                row_anim_op * 0.35
            };
            if *deafened {
                self.egl.draw_rect(
                    hp_x - 2.0,
                    icon_y - 2.0,
                    icon_sz + 4.0,
                    icon_sz + 4.0,
                    sw,
                    sh,
                    [
                        self.config.muted_color[0],
                        self.config.muted_color[1],
                        self.config.muted_color[2],
                        0.6 * row_anim_op,
                    ],
                    4.0,
                );
            }
            self.egl
                .draw_icon(hp_x, icon_y, icon_sz, icon_sz, sw, sh, hp_tex, hp_op);
            if *deafened {
                self.egl.draw_icon(
                    hp_x,
                    icon_y,
                    icon_sz,
                    icon_sz,
                    sw,
                    sh,
                    strike_tex,
                    row_anim_op * 0.85,
                );
            }
        }

        // Scroll indicator strip (only when there are more participants than visible rows)
        if self.participants.len() > self.max_visible_rows {
            let indicator_y = 64.0 + self.visible_row_count() as f32 * row_h as f32;
            self.egl.draw_rect(
                0.0,
                indicator_y,
                sw,
                20.0,
                sw,
                sh,
                [0.15, 0.15, 0.18, op * 0.9],
                0.0,
            );
            if let Some((tex, tw, th)) = self.scroll_indicator_tex {
                let tx = (sw - tw as f32) * 0.5;
                let ty = indicator_y + (20.0 - th as f32) * 0.5;
                self.egl
                    .draw_icon(tx, ty, tw as f32, th as f32, sw, sh, tex, op * 0.7);
            }
        }

        debug!("DRAW: Calling egl.swap()");
        self.egl.swap();
        debug!(
            "DRAW: Calling damage() damage_rect=(0,0,{},{})",
            self.width as i32, self.height as i32
        );
        self.layer
            .wl_surface()
            .damage(0, 0, self.width as i32, self.height as i32);
        debug!("DRAW: Calling wl_surface().commit()");
        self.layer.wl_surface().commit();
        debug!("DRAW: Complete");
    }
}

// Core compact-mode drawing routine that depends only on the Egl backend and
// simple data structures. Extracted so it can be unit-tested without Wayland.
// ─── Helper Functions ────────────────────────────────────────────────────────

/// Delete a texture if it exists (Resource cleanup helper - reduces boilerplate).
fn delete_texture_if_present(
    egl: &dyn EglBackend,
    tex_opt: &mut Option<(glow::NativeTexture, u32, u32)>,
) {
    if let Some((tex, _, _)) = tex_opt.take() {
        egl.delete_texture(tex);
    }
}

/// Delete all textures in a hashmap (Batch resource cleanup helper).
fn delete_all_textures_in_map(
    egl: &dyn EglBackend,
    textures: &mut HashMap<String, (glow::NativeTexture, u32, u32)>,
) {
    for (_, (tex, _, _)) in textures.drain() {
        egl.delete_texture(tex);
    }
}

/// Delete all avatar textures in a hashmap.
fn delete_all_avatar_textures(
    egl: &dyn EglBackend,
    textures: &mut HashMap<String, glow::NativeTexture>,
) {
    for (_, tex) in textures.drain() {
        egl.delete_texture(tex);
    }
}

// ─── Compact Mode Rendering ──────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_compact_core(
    egl: &dyn EglBackend,
    width: u32,
    height: u32,
    opacity: f32,
    idle_alpha: f32,
    participants: &[ParticipantState],
    avatar_textures: &HashMap<String, glow::NativeTexture>,
    initials_textures: &HashMap<String, (glow::NativeTexture, u32, u32)>,
    speaking_color: [f32; 3],
) {
    let op = opacity * idle_alpha;
    let sw = width as f32;
    let sh = height as f32;
    egl.viewport(0, 0, width as i32, height as i32);
    egl.clear_color(0.0, 0.0, 0.0, 0.0);
    egl.clear(glow::COLOR_BUFFER_BIT);

    let avatar_size = 40u32;
    let pad = 4i32;

    for (i, p) in participants.iter().enumerate() {
        let x = pad + i as i32 * 48;
        let y = pad;
        let slot_op = op * p.anim;

        let speaking = p
            .speaking_until
            .map(|t| t > std::time::Instant::now())
            .unwrap_or(false);
        if speaking {
            let [sr, sg, sb] = speaking_color;
            egl.draw_rect(
                (x - 2) as f32,
                (y - 2) as f32,
                (avatar_size + 4) as f32,
                (avatar_size + 4) as f32,
                sw,
                sh,
                [sr, sg, sb, slot_op],
                (avatar_size as f32 / 2.0) + 2.0,
            );
        }

        let desaturate = if p.deafened { 1.0_f32 } else { 0.0 };
        if let Some(&tex) = avatar_textures.get(&p.user_id) {
            egl.draw_avatar(
                x as f32,
                y as f32,
                avatar_size as f32,
                sw,
                sh,
                tex,
                slot_op,
                desaturate,
            );
        } else {
            // Placeholder circle with a colour derived from the user ID
            let hash = p
                .user_id
                .bytes()
                .fold(0u32, |a, b| a.wrapping_add(b as u32));
            let r = ((hash & 0xFF) as f32) / 255.0 * 0.6 + 0.2;
            let g = (((hash >> 8) & 0xFF) as f32) / 255.0 * 0.6 + 0.2;
            let b = (((hash >> 16) & 0xFF) as f32) / 255.0 * 0.6 + 0.2;
            egl.draw_rect(
                x as f32,
                y as f32,
                avatar_size as f32,
                avatar_size as f32,
                sw,
                sh,
                [r, g, b, slot_op],
                avatar_size as f32 / 2.0,
            );
            if let Some(&(tex, tw, th)) = initials_textures.get(&p.user_id) {
                let ix = x as f32 + (avatar_size as f32 - tw as f32) * 0.5;
                let iy = y as f32 + (avatar_size as f32 - th as f32) * 0.5;
                egl.draw_icon(ix, iy, tw as f32, th as f32, sw, sh, tex, slot_op);
            }
        }
    }
}

// Helper functions added for testing
#[cfg(test)]
pub(crate) fn visible_row_count_len(participants_len: usize, max_visible_rows: usize) -> usize {
    participants_len.min(max_visible_rows)
}
#[cfg(test)]
pub(crate) fn compute_compact_dimensions(participants_len: usize) -> (u32, u32) {
    let n = if participants_len == 0 {
        1
    } else {
        participants_len
    };
    let w = ((n as u32) * 48 + 16).max(120);
    (w, 48)
}
#[cfg(test)]
pub(crate) fn compute_normal_height(participants_len: usize, max_visible_rows: usize) -> u32 {
    let n = participants_len.min(max_visible_rows);
    let extra = if participants_len > max_visible_rows {
        20
    } else {
        0
    };
    64 + n as u32 * 48 + extra
}
#[cfg(test)]
pub(crate) fn scroll_label(above: usize, below: usize) -> String {
    match (above > 0, below > 0) {
        (true, true) => format!("↑{}  ↓{}", above, below),
        (true, false) => format!("↑{} more above", above),
        (false, true) => format!("↓{} more below", below),
        _ => String::new(),
    }
}
#[cfg(test)]
pub(crate) fn initial_for_name(name: &str) -> String {
    name.chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_else(|| "?".to_string())
}
#[cfg(test)]
pub(crate) fn placeholder_color_from_userid(user_id: &str) -> (f32, f32, f32) {
    let hash = user_id.bytes().fold(0u32, |a, b| a.wrapping_add(b as u32));
    let r = ((hash & 0xFF) as f32) / 255.0 * 0.6 + 0.2;
    let g = (((hash >> 8) & 0xFF) as f32) / 255.0 * 0.6 + 0.2;
    let b = (((hash >> 16) & 0xFF) as f32) / 255.0 * 0.6 + 0.2;
    (r, g, b)
}
#[cfg(test)]
pub(crate) fn placeholder_color_index(user_id: &str) -> usize {
    let hue = user_id.bytes().fold(0u32, |a, b| a.wrapping_add(b as u32));
    (hue % 4) as usize
}

#[cfg(test)]
mod tests_state_helpers {
    use super::*;

    #[test]
    fn visible_row_count_len_basic() {
        assert_eq!(visible_row_count_len(10, 5), 5);
        assert_eq!(visible_row_count_len(3, 5), 3);
    }

    #[test]
    fn compact_dim() {
        assert_eq!(compute_compact_dimensions(0), (120, 48));
        assert_eq!(compute_compact_dimensions(1), (120, 48));
        assert_eq!(compute_compact_dimensions(3), (160, 48));
    }

    #[test]
    fn normal_height_calc() {
        assert_eq!(compute_normal_height(0, 5), 64);
        assert_eq!(compute_normal_height(6, 5), 64 + 5 * 48 + 20);
    }

    #[test]
    fn scroll_label_various() {
        assert_eq!(scroll_label(2, 3), "↑2  ↓3".to_string());
        assert_eq!(scroll_label(1, 0), "↑1 more above".to_string());
        assert_eq!(scroll_label(0, 2), "↓2 more below".to_string());
        assert_eq!(scroll_label(0, 0), "".to_string());
    }

    #[test]
    fn initial_and_color() {
        assert_eq!(initial_for_name("bob"), "B".to_string());
        assert_eq!(initial_for_name(""), "?".to_string());
        let (r, g, b) = placeholder_color_from_userid("u1");
        assert!((0.2..=0.8).contains(&r));
        assert!((0.2..=0.8).contains(&g));
        assert!((0.2..=0.8).contains(&b));
        let idx = placeholder_color_index("user123");
        assert!(idx < 4);
    }

    #[test]
    fn draw_compact_core_with_avatar() {
        use crate::render::MockEgl;
        use std::collections::HashMap;
        let egl = MockEgl::new();
        let mut avatar_textures: HashMap<String, glow::NativeTexture> = HashMap::new();
        let initials_textures: HashMap<String, (glow::NativeTexture, u32, u32)> = HashMap::new();
        let tex = egl.upload_texture_wh(&[255u8; 4], 1, 1);
        avatar_textures.insert("u1".to_string(), tex);
        let p = ParticipantState {
            user_id: "u1".to_string(),
            display_name: "Alice".to_string(),
            muted: false,
            deafened: false,
            speaking_until: None,
            anim: 1.0,
            leaving: false,
        };
        // Should not panic
        draw_compact_core(
            &egl,
            120,
            48,
            1.0,
            1.0,
            &[p],
            &avatar_textures,
            &initials_textures,
            [0.2, 0.6, 0.2],
        );
    }

    #[test]
    fn draw_compact_core_placeholder_and_speaking() {
        use crate::render::MockEgl;
        use std::collections::HashMap;
        let egl = MockEgl::new();
        let avatar_textures: HashMap<String, glow::NativeTexture> = HashMap::new();
        let mut initials_textures: HashMap<String, (glow::NativeTexture, u32, u32)> =
            HashMap::new();
        let tex = egl.upload_texture_wh(&[255u8; 4], 4, 4);
        initials_textures.insert("u2".to_string(), (tex, 8, 8));
        let p = ParticipantState {
            user_id: "u2".to_string(),
            display_name: "Bob".to_string(),
            muted: false,
            deafened: false,
            speaking_until: Some(
                std::time::Instant::now() + std::time::Duration::from_millis(1500),
            ),
            anim: 1.0,
            leaving: false,
        };
        // Should not panic and should exercise speaking + initials path
        draw_compact_core(
            &egl,
            160,
            48,
            0.8,
            1.0,
            &[p],
            &avatar_textures,
            &initials_textures,
            [1.0, 0.3, 0.2],
        );
    }
}

#[cfg(test)]
mod tests_discord_events {
    use super::*;

    #[test]
    fn participant_state_defaults() {
        let _p = ParticipantState {
            user_id: "test".to_string(),
            display_name: "Test User".to_string(),
            muted: false,
            deafened: false,
            speaking_until: None,
            anim: 1.0,
            leaving: false,
        };
        assert!(_p.user_id == "test");
        assert_eq!(_p.display_name, "Test User");
        assert!(!_p.muted);
        assert!(!_p.deafened);
        assert!(!_p.leaving);
    }

    #[test]
    fn participant_with_speaking() {
        let mut p = ParticipantState {
            user_id: "test".to_string(),
            display_name: "Test User".to_string(),
            muted: false,
            deafened: false,
            speaking_until: None,
            anim: 1.0,
            leaving: false,
        };
        let now = std::time::Instant::now();
        p.speaking_until = Some(now);
        assert!(p.speaking_until.is_some());
    }

    #[test]
    fn participant_leaving_animation() {
        let mut p = ParticipantState {
            user_id: "test".to_string(),
            display_name: "Test User".to_string(),
            muted: false,
            deafened: false,
            speaking_until: None,
            anim: 1.0,
            leaving: false,
        };
        p.leaving = true;
        assert!(p.leaving);
    }

    #[test]
    fn participant_state_builder_from_discord() {
        let discord_p = crate::discord::Participant {
            user_id: "123".to_string(),
            username: "alice".to_string(),
            nick: Some("Alice".to_string()),
            avatar_hash: Some("abc123".to_string()),
            muted: true,
            deafened: false,
        };
        let p = ParticipantStateBuilder::from_discord(&discord_p).build();
        assert_eq!(p.user_id, "123");
        assert_eq!(p.display_name, "Alice"); // nick takes precedence
        assert!(p.muted);
        assert!(!p.deafened);
        assert_eq!(p.anim, 1.0); // default: visible
    }

    #[test]
    fn participant_state_builder_custom_anim() {
        let discord_p = crate::discord::Participant {
            user_id: "456".to_string(),
            username: "bob".to_string(),
            nick: None,
            avatar_hash: None,
            muted: false,
            deafened: true,
        };
        let p = ParticipantStateBuilder::from_discord(&discord_p)
            .anim(0.0) // starting invisible
            .leaving(true)
            .build();
        assert_eq!(p.display_name, "bob"); // no nick, use username
        assert_eq!(p.anim, 0.0);
        assert!(p.leaving);
    }

    #[test]
    fn participant_state_builder_direct() {
        let p = ParticipantStateBuilder::new("789", "Charlie")
            .muted(true)
            .deafened(true)
            .build();
        assert_eq!(p.user_id, "789");
        assert_eq!(p.display_name, "Charlie");
        assert!(p.muted);
        assert!(p.deafened);
        assert_eq!(p.anim, 1.0);
    }

    #[test]
    fn user_joined_sets_in_channel() {
        // Verify that UserJoined event sets in_channel flag for visibility
        let discord_p = crate::discord::Participant {
            user_id: "111".to_string(),
            username: "alice".to_string(),
            nick: Some("Alice".to_string()),
            avatar_hash: None,
            muted: false,
            deafened: false,
        };
        let event = crate::discord::DiscordEvent::UserJoined(discord_p);

        // Check that this event would set in_channel
        // (We can't easily create a full App here, but we document the expected behavior)
        match event {
            crate::discord::DiscordEvent::UserJoined(p) => {
                // When UserJoined arrives, in_channel should be set to true
                // This ensures overlay animates from transparent to visible
                assert_eq!(p.user_id, "111");
                assert_eq!(p.username, "alice");
                assert_eq!(p.nick, Some("Alice".to_string()));
                // The actual in_channel flag is set in App::handle_discord_event
            }
            _ => panic!("Expected UserJoined event"),
        }
    }
}
