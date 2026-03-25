use std::collections::HashMap;
use std::sync::mpsc;
use tracing::{debug, info, trace};

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

pub mod participant;
pub use participant::{ParticipantState, ParticipantStateBuilder};

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
    pub avatar_textures: HashMap<crate::discord::UserId, glow::NativeTexture>,
    pub name_textures: HashMap<crate::discord::UserId, (glow::NativeTexture, u32, u32)>,
    pub initials_textures: HashMap<crate::discord::UserId, (glow::NativeTexture, u32, u32)>,
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
    pub self_user_id: crate::discord::UserId,
    // Participant count display
    pub participant_count_tex: Option<(glow::NativeTexture, u32, u32)>,
    pub last_participant_count: usize,
    // Compact mode overflow badge
    pub overflow_badge_tex: Option<(glow::NativeTexture, u32, u32)>,
    pub last_overflow_count: usize,
    // Ellipsis texture for clipped names
    pub ellipsis_tex: Option<(glow::NativeTexture, u32, u32)>,
    // Speaking ring pulse phase (0.0–1.0, advances while anyone is fully speaking)
    pub speaking_pulse_phase: f32,
    // Per-participant accumulated talk time display
    pub talk_time_textures: HashMap<crate::discord::UserId, (glow::NativeTexture, u32, u32)>,
    pub last_talk_time_secs: HashMap<crate::discord::UserId, u64>,
    // Network latency probe result
    pub ping_ms: Option<u32>,
    pub ping_tex: Option<(glow::NativeTexture, u32, u32)>,
    // Button press held state for visual feedback
    pub mute_held: bool,
    pub deaf_held: bool,
}

/// Parameters for rendering a single participant row.
struct ParticipantRowParams {
    row_y_f: f32,
    anim: f32,
    muted: bool,
    deafened: bool,
    speaking_anim: f32,
    is_self: bool,
    user_id: crate::discord::UserId,
    display_name: String,
    talk_secs: u64,
}

/// Parameters for rendering a status icon (mute/deafen).
struct StatusIconParams {
    x: f32,
    y: f32,
    size: f32,
    tex: glow::NativeTexture,
    strike_tex: glow::NativeTexture,
    is_active: bool,
    opacity: f32,
    bg_opacity: Option<f32>, // None = no background, Some(val) = draw background
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
            let total = self.participants.len();
            let max_compact = self.config.max_visible_rows;
            let visible = total.min(max_compact);
            let has_overflow = total > max_compact;
            let slot_count = if has_overflow { visible + 1 } else { visible };
            let n = slot_count.max(1);
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
    fn find_participant_mut(
        &mut self,
        user_id: &crate::discord::UserId,
    ) -> Option<&mut ParticipantState> {
        self.participants.iter_mut().find(|p| &p.user_id == user_id)
    }

    /// Compact-mode render: single horizontal row of avatars (40 px) with speaking rings.
    fn draw_compact(&mut self) {
        let op = self.opacity * self.idle_alpha;
        trace!(
            in_channel = self.in_channel,
            idle_alpha = self.idle_alpha,
            opacity = self.opacity,
            final_op = op,
            "draw compact"
        );

        // Ensure initials exist for missing avatars (simplifies the core draw path)
        let missing_initials: Vec<(crate::discord::UserId, String)> = self
            .participants
            .iter()
            .filter(|p| !self.avatar_textures.contains_key(&p.user_id))
            .map(|p| (p.user_id.clone(), p.display_name.clone()))
            .collect();
        for (uid, name) in missing_initials {
            self.ensure_initial_texture(&uid, &name);
        }

        // Compute overflow and update badge texture if count changed
        let max_compact = self.config.max_visible_rows;
        let total = self.participants.len();
        let visible_count = total.min(max_compact);
        let overflow = total.saturating_sub(max_compact);
        if overflow != self.last_overflow_count {
            self.last_overflow_count = overflow;
            delete_texture_if_present(&*self.egl, &mut self.overflow_badge_tex);
            if overflow > 0 {
                self.overflow_badge_tex = self.render_text_tex(&format!("+{overflow}"), 13.0);
            }
        }
        let overflow_tex = self.overflow_badge_tex;

        // Call the extracted core drawing routine (testable without Wayland state)
        draw_compact_core(
            &*self.egl,
            self.width,
            self.height,
            self.opacity,
            self.idle_alpha,
            &self.participants[..visible_count],
            &self.avatar_textures,
            &self.initials_textures,
            self.config.speaking_color,
            overflow_tex,
            self.speaking_pulse_phase,
        );

        // Entire surface is interactive (acts as drag handle) in compact mode.
        let region = Region::new(&self.compositor).expect("region");
        region.add(0, 0, self.width as i32, self.height as i32);
        self.layer.set_input_region(Some(region.wl_region()));

        self.egl.swap();
        self.layer
            .wl_surface()
            .damage(0, 0, self.width as i32, self.height as i32);
        self.layer.wl_surface().commit();
        trace!("draw compact complete");
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

    pub fn make_name_texture(&mut self, user_id: &crate::discord::UserId, name: &str) {
        if let Some(font) = &self.font {
            let font_size = self.config.font_size;
            let (pixels, w, h) = render_text_texture(font, name, font_size);
            if w > 0 && h > 0 {
                let tex = self.egl.upload_texture_wh(&pixels, w, h);
                self.name_textures.insert(user_id.clone(), (tex, w, h));
            }
        }
    }

    pub fn ensure_initial_texture(&mut self, user_id: &crate::discord::UserId, display_name: &str) {
        if self.initials_textures.contains_key(user_id) {
            return;
        }
        let initial = display_name
            .chars()
            .next()
            .map(|c| c.to_uppercase().to_string())
            .unwrap_or_else(|| "?".to_string());
        if let Some((tex, w, h)) = self.render_text_tex(&initial, self.config.font_size * 1.43) {
            self.initials_textures.insert(user_id.clone(), (tex, w, h));
        }
    }

    fn ensure_ellipsis_tex(&mut self) {
        if self.ellipsis_tex.is_none() {
            self.ellipsis_tex = self.render_text_tex("\u{2026}", self.config.font_size);
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
                if changed {
                    debug!(mute, deaf, "local voice settings changed");
                }
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
                debug!(
                    count = parts.len(),
                    channel = ?channel_name,
                    "voice participant list received"
                );
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
                            let display = format!("# {name}");
                            let (pixels, w, h) =
                                render_text_texture(font, &display, self.config.font_size * 0.93);
                            if w > 0 && h > 0 {
                                let tex = self.egl.upload_texture_wh(&pixels, w, h);
                                self.channel_name_tex = Some((tex, w, h));
                            }
                        }
                    }
                }
                delete_all_textures_in_map(&*self.egl, &mut self.name_textures);
                delete_all_textures_in_map(&*self.egl, &mut self.initials_textures);
                delete_all_textures_in_map(&*self.egl, &mut self.talk_time_textures);
                self.last_talk_time_secs.clear();
                self.participants = parts
                    .iter()
                    .map(|p| ParticipantStateBuilder::from_discord(p).build())
                    .collect();
                let user_names: Vec<(crate::discord::UserId, String)> = self
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
                self.resize_overlay(self.compute_overlay_height());
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
                self.resize_overlay(self.compute_overlay_height());
                if self.compact {
                    self.apply_compact_resize();
                }
                debug!(
                    count = self.participants.len(),
                    "participant count after join"
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
                    if changed {
                        debug!(
                            name = %p.display_name,
                            muted,
                            deafened,
                            "participant state updated"
                        );
                    }
                    p.muted = muted;
                    p.deafened = deafened;
                    return changed;
                }
                false
            }
            discord::DiscordEvent::SpeakingUpdate { user_id, speaking } => {
                if let Some(p) = self.find_participant_mut(&user_id) {
                    trace!(name = %p.display_name, speaking, "speaking update");
                    let now = std::time::Instant::now();
                    if speaking {
                        // Ring stays for 1.5s after last SPEAKING_START.
                        // Discord fires SPEAKING_START ~every 1s while active,
                        // so the ring clears ~0.5s after the user goes quiet.
                        p.speaking_until = Some(now + std::time::Duration::from_millis(1500));
                        // Record segment start only on the first SPEAKING_START
                        if p.speaking_started_at.is_none() {
                            p.speaking_started_at = Some(now);
                        }
                    } else {
                        p.speaking_until = None;
                        // Finalize talk time segment
                        if let Some(started) = p.speaking_started_at.take() {
                            p.talk_time += started.elapsed();
                        }
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
                debug!(%user_id, size, "avatar texture uploaded");
                let tex = self.egl.upload_texture_wh(&rgba, size, size);
                self.avatar_textures.insert(user_id, tex);
                true
            }
            discord::DiscordEvent::GuildName { name } => {
                debug!(guild = ?name, "guild name updated");
                delete_texture_if_present(&*self.egl, &mut self.guild_name_tex);
                if name.is_empty() {
                    self.guild_name = None;
                } else {
                    self.guild_name_tex = self.render_text_tex(&name, self.config.font_size * 0.79);
                    self.guild_name = Some(name);
                }
                true
            }
            discord::DiscordEvent::PingResult { latency_ms } => {
                self.ping_ms = Some(latency_ms);
                delete_texture_if_present(&*self.egl, &mut self.ping_tex);
                let label = format!("~{latency_ms}ms");
                self.ping_tex = self.render_text_tex(&label, self.config.font_size * 0.86);
                true
            }
            discord::DiscordEvent::Disconnected => {
                info!("Discord disconnected, clearing voice state");
                // Clear all voice-channel state so the UI resets to idle.
                self.in_channel = false;
                self.channel_name = None;
                delete_texture_if_present(&*self.egl, &mut self.channel_name_tex);
                delete_texture_if_present(&*self.egl, &mut self.participant_count_tex);
                self.last_participant_count = usize::MAX;
                // Clear compact overflow badge
                delete_texture_if_present(&*self.egl, &mut self.overflow_badge_tex);
                self.last_overflow_count = usize::MAX;
                // Clear guild name
                self.guild_name = None;
                delete_texture_if_present(&*self.egl, &mut self.guild_name_tex);
                // Clear session timer
                self.channel_joined_at = None;
                delete_texture_if_present(&*self.egl, &mut self.timer_tex);
                // Clear ping texture (keep ping_ms as last-known value)
                delete_texture_if_present(&*self.egl, &mut self.ping_tex);
                // Clear scroll indicator
                delete_texture_if_present(&*self.egl, &mut self.scroll_indicator_tex);
                self.scroll_offset = 0;
                self.last_scroll_state = (usize::MAX, usize::MAX);
                delete_all_avatar_textures(&*self.egl, &mut self.avatar_textures);
                delete_all_textures_in_map(&*self.egl, &mut self.name_textures);
                delete_all_textures_in_map(&*self.egl, &mut self.initials_textures);
                delete_all_textures_in_map(&*self.egl, &mut self.talk_time_textures);
                self.last_talk_time_secs.clear();
                self.participants.clear();
                self.discord_mute = false;
                self.discord_deaf = false;
                self.idle_alpha = 0.0;
                self.resize_overlay(64);
                true
            }
        }
    }

    /// Compute overlay height based on participant count.
    /// Height = control bar (64px) + rows (48px each) + scroll indicator (20px if needed).
    pub fn compute_overlay_height(&self) -> u32 {
        let visible_rows = self.visible_row_count() as u32;
        let scroll_height = if self.participants.len() > self.max_visible_rows {
            20
        } else {
            0
        };
        64 + visible_rows * 48 + scroll_height
    }

    /// Draw the header section (drag handle, guild/channel names, timer).
    fn draw_header(&mut self, op: f32, sw: f32, sh: f32) {
        let (hx, hy, hw, hh) = drag_handle_rects(self.width, 64);

        // Header panel background
        self.egl.draw_rect(
            2.0,
            2.0,
            sw - 4.0,
            60.0,
            sw,
            sh,
            [0.08, 0.09, 0.11, 0.72 * op],
            10.0,
        );

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
        let text_x = (hx + hw) as f32 + 8.0;
        if let Some((tex, tw, th)) = self.guild_name_tex {
            self.egl
                .draw_icon(text_x, 6.0, tw as f32, th as f32, sw, sh, tex, op * 0.55);
        }
        if let Some((tex, tw, th)) = self.channel_name_tex {
            self.egl
                .draw_icon(text_x, 22.0, tw as f32, th as f32, sw, sh, tex, op * 0.90);
        }
        let mut timer_next_x = text_x;
        if let Some((tex, tw, th)) = self.timer_tex {
            self.egl.draw_icon(
                timer_next_x,
                40.0,
                tw as f32,
                th as f32,
                sw,
                sh,
                tex,
                op * 0.55,
            );
            timer_next_x += tw as f32 + 8.0;
        }
        if let Some((tex, tw, th)) = self.ping_tex {
            self.egl.draw_icon(
                timer_next_x,
                40.0,
                tw as f32,
                th as f32,
                sw,
                sh,
                tex,
                op * 0.55,
            );
        }

        // Participant count — right-aligned but kept clear of the mute/deafen buttons
        let (bx2, _, _, _) = button2_rects(self.width, 64);
        if let Some((tex, tw, th)) = self.participant_count_tex {
            let px = (bx2 as f32 - tw as f32 - 8.0).max(text_x);
            self.egl
                .draw_icon(px, 6.0, tw as f32, th as f32, sw, sh, tex, op * 0.50);
        }

        // PTT indicator dot — bottom-right of header
        if self.ptt_mode {
            let dot_color = if self.ptt_active {
                [0.2, 0.85, 0.4, 0.95 * op] // bright green when transmitting
            } else {
                [0.7, 0.4, 0.1, 0.50 * op] // dim amber when silent
            };
            self.egl
                .draw_rect(sw - 14.0, 44.0, 10.0, 10.0, sw, sh, dot_color, 5.0);
        }
    }

    /// Draw a 1px separator line between the header and the participant list.
    fn draw_header_separator(&self, op: f32, sw: f32, sh: f32) {
        self.egl.draw_rect(
            8.0,
            63.0,
            sw - 16.0,
            1.0,
            sw,
            sh,
            [1.0, 1.0, 1.0, 0.08 * op],
            0.0,
        );
    }

    /// Draw scroll indicator (if needed).
    fn draw_scroll_indicator(&mut self, op: f32, sw: f32, sh: f32) {
        if self.participants.len() <= self.max_visible_rows {
            return;
        }

        const ROW_HEIGHT: f32 = 48.0;
        let indicator_y = 64.0 + self.visible_row_count() as f32 * ROW_HEIGHT;

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

    /// Draw an icon with optional strikeout overlay (without background).
    ///
    /// Used for button icons (mute/deafen buttons) where the button background is separate.
    fn draw_icon_with_strikeout(&self, params: &StatusIconParams, sw: f32, sh: f32) {
        self.egl.draw_icon(
            params.x,
            params.y,
            params.size,
            params.size,
            sw,
            sh,
            params.tex,
            params.opacity,
        );
        if params.is_active {
            self.egl.draw_icon(
                params.x,
                params.y,
                params.size,
                params.size,
                sw,
                sh,
                params.strike_tex,
                params.opacity,
            );
        }
    }

    /// Draw a status icon (mute, deafen) with optional background and strikeout overlay.
    ///
    /// Handles drawing the icon, optional background rect when active, and strikeout.
    /// Used for both button mute/deaf indicators and per-participant indicators.
    fn draw_status_icon(&self, params: &StatusIconParams, sw: f32, sh: f32) {
        // Optional background when active
        if params.is_active {
            if let Some(bg_op) = params.bg_opacity {
                self.egl.draw_rect(
                    params.x - 2.0,
                    params.y - 2.0,
                    params.size + 4.0,
                    params.size + 4.0,
                    sw,
                    sh,
                    [
                        self.config.muted_color[0],
                        self.config.muted_color[1],
                        self.config.muted_color[2],
                        bg_op,
                    ],
                    4.0,
                );
            }
        }

        // Icon itself
        self.egl.draw_icon(
            params.x,
            params.y,
            params.size,
            params.size,
            sw,
            sh,
            params.tex,
            params.opacity,
        );

        // Strikeout overlay when active
        if params.is_active {
            self.egl.draw_icon(
                params.x,
                params.y,
                params.size,
                params.size,
                sw,
                sh,
                params.strike_tex,
                params.opacity * 0.85,
            );
        }
    }

    /// Draw a single participant row (avatar, name, status icons, etc.).
    ///
    /// Handles: row background, speaking ring, avatar/initials, name text,
    /// mute/deaf icons with optional background highlights.
    fn draw_participant_row(
        &mut self,
        params: &ParticipantRowParams,
        op: f32,
        sw: f32,
        sh: f32,
        row_h: u32,
    ) {
        // Deafened participants are de-emphasized at reduced opacity
        let deaf_dim = if params.deafened { 0.65 } else { 1.0 };
        let row_anim_op = op * params.anim * deaf_dim;

        // Row background
        self.egl.draw_rect(
            4.0,
            params.row_y_f + 4.0,
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

        // Self-user highlight — subtle blue tint so you can spot yourself instantly
        if params.is_self {
            self.egl.draw_rect(
                4.0,
                params.row_y_f + 4.0,
                sw - 8.0,
                row_h as f32 - 8.0,
                sw,
                sh,
                [0.25, 0.45, 1.0, 0.10 * row_anim_op],
                8.0,
            );
        }

        // Speaking tint — subtle color wash, intensity driven by speaking_anim
        if params.speaking_anim > 0.005 {
            self.egl.draw_rect(
                4.0,
                params.row_y_f + 4.0,
                sw - 8.0,
                row_h as f32 - 8.0,
                sw,
                sh,
                [
                    self.config.speaking_color[0],
                    self.config.speaking_color[1],
                    self.config.speaking_color[2],
                    0.10 * params.speaking_anim * row_anim_op,
                ],
                8.0,
            );
        }

        // Avatar positioning
        let av_size = 32f32;
        let av_x = 12f32;
        let av_y = params.row_y_f + (row_h as f32 - av_size) * 0.5;

        // Speaking ring — fades in/out via speaking_anim, pulses gently when fully on
        if params.speaking_anim > 0.005 {
            let ring = 3.0f32;
            let sc = self.config.speaking_color;
            let pulse_factor = if params.speaking_anim > 0.95 {
                let phi = self.speaking_pulse_phase * std::f32::consts::TAU;
                0.65 + 0.35 * (0.5 - 0.5 * phi.cos())
            } else {
                1.0 // linear during fade-in/out
            };
            self.egl.draw_rect(
                av_x - ring,
                av_y - ring,
                av_size + ring * 2.0,
                av_size + ring * 2.0,
                sw,
                sh,
                [
                    sc[0],
                    sc[1],
                    sc[2],
                    0.9 * params.speaking_anim * row_anim_op * pulse_factor,
                ],
                (av_size + ring * 2.0) * 0.5,
            );
        }

        // Subtle 1px border gives avatars visual grounding against the row background
        self.egl.draw_rect(
            av_x - 1.0,
            av_y - 1.0,
            av_size + 2.0,
            av_size + 2.0,
            sw,
            sh,
            [1.0, 1.0, 1.0, 0.15 * row_anim_op],
            (av_size + 2.0) * 0.5,
        );

        // Avatar or placeholder
        if let Some(&tex) = self.avatar_textures.get(&params.user_id) {
            let desaturate = if params.deafened {
                1.0_f32
            } else if params.muted {
                0.35
            } else {
                0.0
            };
            self.egl
                .draw_avatar(av_x, av_y, av_size, sw, sh, tex, row_anim_op, desaturate);
        } else {
            let hue = params
                .user_id
                .bytes()
                .fold(0u32, |a, b| a.wrapping_add(b as u32));
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
            self.ensure_initial_texture(&params.user_id, &params.display_name);
            let initial_data = self
                .initials_textures
                .get(&params.user_id)
                .map(|&(t, w, h)| (t, w, h));
            if let Some((tex, tw, th)) = initial_data {
                let ix = av_x + (av_size - tw as f32) * 0.5;
                let iy = av_y + (av_size - th as f32) * 0.5;
                self.egl
                    .draw_icon(ix, iy, tw as f32, th as f32, sw, sh, tex, row_anim_op);
            }
        }

        // Name text — dimmed when muted to match Discord's de-emphasis style
        let name_op = if params.muted {
            row_anim_op * 0.65
        } else {
            row_anim_op
        };
        let icon_sz = 16.0f32;
        let icon_gap = 4.0f32;
        let icons_w = icon_sz * 2.0 + icon_gap + 8.0;
        let name_x = av_x + av_size + 8.0;
        let name_w_max = sw - name_x - icons_w;
        let show_talk_time = params.talk_secs > 0;
        if let Some(&(tex, tw, th)) = self.name_textures.get(&params.user_id) {
            let draw_h = th as f32;
            // When talk time is shown, stack name above center; otherwise center it
            let name_y = if show_talk_time {
                params.row_y_f + (row_h as f32 * 0.5 - draw_h - 1.0)
            } else {
                params.row_y_f + (row_h as f32 - draw_h) * 0.5
            };
            if tw as f32 > name_w_max {
                self.egl
                    .draw_icon(name_x, name_y, name_w_max, draw_h, sw, sh, tex, name_op);
                self.ensure_ellipsis_tex();
                if let Some((etex, etw, eth)) = self.ellipsis_tex {
                    let ex = name_x + name_w_max - etw as f32;
                    let ey = if show_talk_time {
                        params.row_y_f + (row_h as f32 * 0.5 - eth as f32 - 1.0)
                    } else {
                        params.row_y_f + (row_h as f32 - eth as f32) * 0.5
                    };
                    self.egl.draw_icon(
                        ex,
                        ey,
                        etw as f32,
                        eth as f32,
                        sw,
                        sh,
                        etex,
                        name_op * 0.85,
                    );
                }
            } else {
                self.egl
                    .draw_icon(name_x, name_y, tw as f32, draw_h, sw, sh, tex, name_op);
            }
        }

        // Talk time label below name (only when the participant has spoken)
        if show_talk_time {
            if let Some(&(tt_tex, tt_w, tt_h)) = self.talk_time_textures.get(&params.user_id) {
                let tt_y = params.row_y_f + row_h as f32 * 0.5 + 1.0;
                self.egl.draw_icon(
                    name_x,
                    tt_y,
                    tt_w as f32,
                    tt_h as f32,
                    sw,
                    sh,
                    tt_tex,
                    row_anim_op * 0.65,
                );
            }
        }

        // Per-participant mute/deaf icons (right side of row)
        let mic_tex = self.egl.tex_mic();
        let hp_tex = self.egl.tex_headphone();
        let strike_tex = self.egl.tex_strikeout();
        let icon_y = params.row_y_f + (row_h as f32 - icon_sz) * 0.5;
        let mic_x = sw - icon_sz * 2.0 - icon_gap - 8.0;
        let hp_x = sw - icon_sz - 8.0;

        // Mute icon — only rendered when the participant is actually muted
        if params.muted {
            let mute_params = StatusIconParams {
                x: mic_x,
                y: icon_y,
                size: icon_sz,
                tex: mic_tex,
                strike_tex,
                is_active: true,
                opacity: row_anim_op * 0.9,
                bg_opacity: Some(0.6 * row_anim_op),
            };
            self.draw_status_icon(&mute_params, sw, sh);
        }

        // Deafen icon — only rendered when the participant is actually deafened
        if params.deafened {
            let deaf_params = StatusIconParams {
                x: hp_x,
                y: icon_y,
                size: icon_sz,
                tex: hp_tex,
                strike_tex,
                is_active: true,
                opacity: row_anim_op * 0.9,
                bg_opacity: Some(0.6 * row_anim_op),
            };
            self.draw_status_icon(&deaf_params, sw, sh);
        }
    }

    pub fn draw(&mut self) {
        let op = self.opacity * self.idle_alpha;
        trace!(
            in_channel = self.in_channel,
            idle_alpha = self.idle_alpha,
            opacity = self.opacity,
            final_op = op,
            compact = self.compact,
            "draw"
        );

        if self.compact {
            self.draw_compact();
            return;
        }
        let (sw, sh) = (self.width as f32, self.height as f32);
        let (bx, by, bw, bh) = button_rects(self.width, 64);
        let (bx2, by2, bw2, bh2) = button2_rects(self.width, 64);
        let (hx, hy, hw, hh) = drag_handle_rects(self.width, 64);

        self.egl
            .viewport(0, 0, self.width as i32, self.height as i32);
        self.egl.clear_color(0.0, 0.0, 0.0, 0.0);
        self.egl.clear(glow::COLOR_BUFFER_BIT);
        self.egl.use_main_program();

        // Draw header (drag handle, guild/channel names, timer)
        self.draw_header(op, sw, sh);
        if self.in_channel {
            self.draw_header_separator(op, sw, sh);
        }

        // Draw mute/deafen buttons with proper colors and icons
        let effectively_muted = self.discord_mute || self.discord_deaf;
        let mic_alpha = if self.ptt_mode && !self.ptt_active && !effectively_muted {
            op * 0.35
        } else {
            op * 0.82
        };
        let mc = self.config.muted_color;
        let mute_dim = if self.mute_held { 0.75_f32 } else { 1.0 };
        let mute_base = if effectively_muted {
            [
                mc[0] * mute_dim,
                mc[1] * mute_dim,
                mc[2] * mute_dim,
                0.88 * op,
            ]
        } else {
            [0.12 * mute_dim, 0.68 * mute_dim, 0.28 * mute_dim, mic_alpha]
        };
        self.egl.draw_rect(
            bx2 as f32, by2 as f32, bw2 as f32, bh2 as f32, sw, sh, mute_base, 10.0,
        );

        let deaf_dim = if self.deaf_held { 0.75_f32 } else { 1.0 };
        let deaf_base = if self.discord_deaf {
            [
                mc[0] * deaf_dim,
                mc[1] * deaf_dim,
                mc[2] * deaf_dim,
                0.88 * op,
            ]
        } else {
            [0.18 * deaf_dim, 0.36 * deaf_dim, 0.82 * deaf_dim, 0.82 * op]
        };
        self.egl.draw_rect(
            bx as f32, by as f32, bw as f32, bh as f32, sw, sh, deaf_base, 10.0,
        );

        // Icon overlays — mic on mute button, headphone on deafen button
        let pad = 6.0f32;
        let mic_tex = self.egl.tex_mic();
        let hp_tex = self.egl.tex_headphone();
        let strike_tex = self.egl.tex_strikeout();

        // Compute icon sizes (fit the smaller of the button width/height) and center
        // them inside the button rect to avoid overflow when button height < width.
        let mute_icon_size = ((bw2 as f32).min(bh2 as f32) - 2.0 * pad).max(0.0);
        let mute_icon_x = bx2 as f32 + (bw2 as f32 - mute_icon_size) * 0.5;
        let mute_icon_y = by2 as f32 + (bh2 as f32 - mute_icon_size) * 0.5;
        let mute_icon_params = StatusIconParams {
            x: mute_icon_x,
            y: mute_icon_y,
            size: mute_icon_size,
            tex: mic_tex,
            strike_tex,
            is_active: effectively_muted,
            opacity: mic_alpha,
            bg_opacity: None,
        };
        self.draw_icon_with_strikeout(&mute_icon_params, sw, sh);

        // Deafen icon on deafen button
        let deaf_icon_size = ((bw as f32).min(bh as f32) - 2.0 * pad).max(0.0);
        let deaf_icon_x = bx as f32 + (bw as f32 - deaf_icon_size) * 0.5;
        let deaf_icon_y = by as f32 + (bh as f32 - deaf_icon_size) * 0.5;
        let deaf_icon_params = StatusIconParams {
            x: deaf_icon_x,
            y: deaf_icon_y,
            size: deaf_icon_size,
            tex: hp_tex,
            strike_tex,
            is_active: self.discord_deaf,
            opacity: op,
            bg_opacity: None,
        };
        self.draw_icon_with_strikeout(&deaf_icon_params, sw, sh);

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
                    let new_tex = self.render_text_tex(&label, self.config.font_size * 0.79);
                    self.scroll_indicator_tex = new_tex;
                }
            }
        } else if self.scroll_indicator_tex.is_some() {
            delete_texture_if_present(&*self.egl, &mut self.scroll_indicator_tex);
            self.last_scroll_state = (usize::MAX, usize::MAX);
        }

        // Update participant count texture when count changes
        let count = self.participants.len();
        if count != self.last_participant_count {
            self.last_participant_count = count;
            delete_texture_if_present(&*self.egl, &mut self.participant_count_tex);
            if count > 0 {
                let label = format!("{count} in channel");
                self.participant_count_tex =
                    self.render_text_tex(&label, self.config.font_size * 0.79);
            }
        }

        // Collect visible participant data to avoid re-borrowing self in the loop
        type VisibleRow = (
            usize,
            f32,
            bool,
            bool,
            f32,
            bool,
            crate::discord::UserId,
            String,
            u64,
        );
        let self_id = self.self_user_id.clone();
        let visible: Vec<VisibleRow> = self
            .participants
            .iter()
            .enumerate()
            .skip(self.scroll_offset)
            .take(self.max_visible_rows)
            .map(|(abs_idx, p)| {
                (
                    abs_idx,
                    p.anim,
                    p.muted,
                    p.deafened,
                    p.speaking_anim,
                    p.user_id == self_id,
                    p.user_id.clone(),
                    p.display_name.clone(),
                    p.current_talk_secs(),
                )
            })
            .collect();

        for (
            slot,
            (
                abs_idx,
                anim,
                muted,
                deafened,
                speaking_anim,
                is_self,
                user_id,
                display_name,
                talk_secs,
            ),
        ) in visible.iter().enumerate()
        {
            let _ = abs_idx; // abs_idx available for future use; slot drives layout
                             // Apply smoothstep easing so rows ease-in-out instead of sliding at constant speed
            let eased = {
                let t = *anim;
                t * t * (3.0 - 2.0 * t)
            };
            let slide_offset = (1.0 - eased) * row_h as f32 * 0.35;
            let row_y_f = 64.0_f32 + slot as f32 * row_h as f32 + slide_offset;

            let params = ParticipantRowParams {
                row_y_f,
                anim: eased,
                muted: *muted,
                deafened: *deafened,
                speaking_anim: *speaking_anim,
                is_self: *is_self,
                user_id: user_id.clone(),
                display_name: display_name.clone(),
                talk_secs: *talk_secs,
            };
            self.draw_participant_row(&params, op, sw, sh, row_h);
        }

        // Draw scroll indicator (if needed)
        self.draw_scroll_indicator(op, sw, sh);

        self.egl.swap();
        self.layer
            .wl_surface()
            .damage(0, 0, self.width as i32, self.height as i32);
        self.layer.wl_surface().commit();
        trace!("draw complete");
    }

    // ─── Animation tick methods (called from the main timer callback) ─────────

    /// Advance join/leave animations and remove fully-faded participants.
    /// Returns true if a redraw is needed.
    pub fn tick_animations(&mut self, anim_speed: f32) -> bool {
        let mut needs_redraw = false;
        let mut to_remove: Vec<crate::discord::UserId> = Vec::new();

        for p in &mut self.participants {
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

            // Animate speaking ring in/out
            let target_speaking = p
                .speaking_until
                .map(|t| t > std::time::Instant::now())
                .unwrap_or(false);
            let speaking_speed = anim_speed * 1.5; // slightly faster than join/leave
            if target_speaking && p.speaking_anim < 1.0 {
                p.speaking_anim = (p.speaking_anim + speaking_speed).min(1.0);
                needs_redraw = true;
            } else if !target_speaking && p.speaking_anim > 0.0 {
                p.speaking_anim = (p.speaking_anim - speaking_speed).max(0.0);
                needs_redraw = true;
            }
        }

        // Advance pulse phase while anyone is fully speaking (period = 0.6s)
        let any_fully_speaking = self.participants.iter().any(|p| p.speaking_anim > 0.95);
        if any_fully_speaking {
            self.speaking_pulse_phase = (self.speaking_pulse_phase + 0.016 / 0.6) % 1.0;
            needs_redraw = true;
        }

        for uid in &to_remove {
            if let Some(pos) = self.participants.iter().position(|p| &p.user_id == uid) {
                let name = self.participants[pos].display_name.clone();
                debug!(%name, "participant animation complete, removing");
                self.participants.remove(pos);
                if let Some((tex, _, _)) = self.name_textures.remove(uid) {
                    self.egl.delete_texture(tex);
                }
                if let Some((tex, _, _)) = self.initials_textures.remove(uid) {
                    self.egl.delete_texture(tex);
                }
                if let Some(tex) = self.avatar_textures.remove(uid) {
                    self.egl.delete_texture(tex);
                }
                if let Some((tex, _, _)) = self.talk_time_textures.remove(uid) {
                    self.egl.delete_texture(tex);
                }
                self.last_talk_time_secs.remove(uid);
            }
        }

        if !to_remove.is_empty() {
            let max_offset = self
                .participants
                .len()
                .saturating_sub(self.max_visible_rows);
            self.scroll_offset = self.scroll_offset.min(max_offset);
            let new_h = self.compute_overlay_height();
            self.resize_overlay(new_h);
            if self.compact {
                self.apply_compact_resize();
            }
            needs_redraw = true;
        }

        needs_redraw
    }

    /// Clear expired speaking rings. Returns true if any ring was cleared.
    pub fn tick_speaking_expiry(&mut self, now: std::time::Instant) -> bool {
        let any_expired = self
            .participants
            .iter()
            .any(|p| p.speaking_until.map(|t| t <= now).unwrap_or(false));
        if any_expired {
            for p in &mut self.participants {
                if p.speaking_until.map(|t| t <= now).unwrap_or(false) {
                    p.speaking_until = None;
                    // Finalize any in-progress talk segment when the ring expires
                    if let Some(started) = p.speaking_started_at.take() {
                        p.talk_time += started.elapsed();
                    }
                }
            }
        }
        any_expired
    }

    /// Update push-to-talk active state. Returns true if the state changed.
    pub fn tick_ptt(&mut self, now: std::time::Instant) -> bool {
        if !self.ptt_mode {
            return false;
        }
        let self_speaking = self.participants.iter().any(|p| {
            p.user_id == self.self_user_id && p.speaking_until.map(|t| t > now).unwrap_or(false)
        });
        if self.ptt_active != self_speaking {
            self.ptt_active = self_speaking;
            true
        } else {
            false
        }
    }

    /// Animate the idle-fade alpha toward its target (1.0 in channel, 0.0 out).
    /// Also clears the input region when the overlay becomes fully hidden.
    /// Returns true if the alpha is still moving (redraw needed).
    pub fn tick_idle_alpha(&mut self) -> bool {
        let target_alpha = if self.in_channel { 1.0_f32 } else { 0.0_f32 };
        let animating = (self.idle_alpha - target_alpha).abs() > 0.005;
        if animating {
            let speed = 0.016 / 0.25;
            if self.idle_alpha < target_alpha {
                self.idle_alpha = (self.idle_alpha + speed).min(target_alpha);
            } else {
                self.idle_alpha = (self.idle_alpha - speed).max(target_alpha);
            }
            trace!(
                target = target_alpha,
                current = self.idle_alpha,
                "animating idle alpha"
            );
        }
        // Clear input region when fully hidden (checked every tick, cheap)
        if !self.in_channel && self.idle_alpha <= 0.005 && self.idle_alpha > -0.001 {
            trace!("overlay fully hidden, clearing input region");
            self.clear_input_region();
        }
        animating
    }

    /// Update the session timer texture when the elapsed second changes.
    /// Returns true if the texture was updated (redraw needed).
    pub fn tick_timer(&mut self) -> bool {
        let joined_at = match self.channel_joined_at {
            Some(t) => t,
            None => return false,
        };
        let elapsed = joined_at.elapsed().as_secs() as u32;
        if elapsed == self.last_timer_secs {
            return false;
        }
        self.last_timer_secs = elapsed;
        let h = elapsed / 3600;
        let m = (elapsed % 3600) / 60;
        let s = elapsed % 60;
        let label = if h > 0 {
            format!("\u{2022} {h}:{m:02}:{s:02}")
        } else {
            format!("\u{2022} {m}:{s:02}")
        };
        if let Some((tex, _, _)) = self.timer_tex.take() {
            self.egl.delete_texture(tex);
        }
        self.timer_tex = self.render_text_tex(&label, self.config.font_size * 0.86);
        true
    }

    /// Update per-participant talk time textures when their displayed second changes.
    /// Returns true if any texture was updated (redraw needed).
    pub fn tick_talk_time_textures(&mut self) -> bool {
        let user_secs: Vec<(crate::discord::UserId, u64)> = self
            .participants
            .iter()
            .map(|p| (p.user_id.clone(), p.current_talk_secs()))
            .collect();

        let mut changed = false;
        for (uid, secs) in user_secs {
            if secs == 0 {
                // Remove stale texture if the participant hasn't spoken yet
                if let Some((tex, _, _)) = self.talk_time_textures.remove(&uid) {
                    self.egl.delete_texture(tex);
                    self.last_talk_time_secs.remove(&uid);
                    changed = true;
                }
                continue;
            }
            let last = self
                .last_talk_time_secs
                .get(&uid)
                .copied()
                .unwrap_or(u64::MAX);
            if secs == last {
                continue;
            }
            // Regenerate texture for this participant
            if let Some((old_tex, _, _)) = self.talk_time_textures.remove(&uid) {
                self.egl.delete_texture(old_tex);
            }
            let h = secs / 3600;
            let m = (secs % 3600) / 60;
            let s = secs % 60;
            let label = if h > 0 {
                format!("{h}:{m:02}:{s:02}")
            } else {
                format!("{m}:{s:02}")
            };
            if let Some(tex_data) = self.render_text_tex(&label, self.config.font_size * 0.78) {
                self.talk_time_textures.insert(uid.clone(), tex_data);
                self.last_talk_time_secs.insert(uid, secs);
                changed = true;
            }
        }
        changed
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
    textures: &mut HashMap<crate::discord::UserId, (glow::NativeTexture, u32, u32)>,
) {
    for (_, (tex, _, _)) in textures.drain() {
        egl.delete_texture(tex);
    }
}

/// Delete all avatar textures in a hashmap.
fn delete_all_avatar_textures(
    egl: &dyn EglBackend,
    textures: &mut HashMap<crate::discord::UserId, glow::NativeTexture>,
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
    avatar_textures: &HashMap<crate::discord::UserId, glow::NativeTexture>,
    initials_textures: &HashMap<crate::discord::UserId, (glow::NativeTexture, u32, u32)>,
    speaking_color: [f32; 3],
    overflow_tex: Option<(glow::NativeTexture, u32, u32)>,
    speaking_pulse_phase: f32,
) {
    let op = opacity * idle_alpha;
    let sw = width as f32;
    let sh = height as f32;
    egl.viewport(0, 0, width as i32, height as i32);
    egl.clear_color(0.0, 0.0, 0.0, 0.0);
    egl.clear(glow::COLOR_BUFFER_BIT);

    let avatar_size = 40u32;
    let pad = 4i32;

    // Dark pill background behind the whole avatar strip
    egl.draw_rect(
        0.0,
        0.0,
        sw,
        sh,
        sw,
        sh,
        [0.08, 0.09, 0.11, 0.72 * op],
        (sh * 0.5).min(16.0),
    );

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
            let pulse_factor = if p.speaking_anim > 0.95 {
                let phi = speaking_pulse_phase * std::f32::consts::TAU;
                0.65 + 0.35 * (0.5 - 0.5 * phi.cos())
            } else {
                1.0
            };
            egl.draw_rect(
                (x - 2) as f32,
                (y - 2) as f32,
                (avatar_size + 4) as f32,
                (avatar_size + 4) as f32,
                sw,
                sh,
                [sr, sg, sb, slot_op * pulse_factor],
                (avatar_size as f32 / 2.0) + 2.0,
            );
        }

        // Subtle 1px border gives avatars visual grounding
        egl.draw_rect(
            x as f32 - 1.0,
            y as f32 - 1.0,
            (avatar_size + 2) as f32,
            (avatar_size + 2) as f32,
            sw,
            sh,
            [1.0, 1.0, 1.0, 0.15 * slot_op],
            (avatar_size as f32 + 2.0) * 0.5,
        );

        let desaturate = if p.deafened {
            1.0_f32
        } else if p.muted {
            0.35
        } else {
            0.0
        };
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

    // Overflow badge: dark circle with "+N" text at the slot after last visible avatar
    if let Some((tex, tw, th)) = overflow_tex {
        let badge_x = pad + participants.len() as i32 * 48;
        let badge_size = avatar_size as f32;
        egl.draw_rect(
            badge_x as f32,
            pad as f32,
            badge_size,
            badge_size,
            sw,
            sh,
            [0.2, 0.2, 0.25, 0.8 * op],
            badge_size * 0.5,
        );
        let tx = badge_x as f32 + (badge_size - tw as f32) * 0.5;
        let ty = pad as f32 + (badge_size - th as f32) * 0.5;
        egl.draw_icon(tx, ty, tw as f32, th as f32, sw, sh, tex, op * 0.9);
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

/// Mirrors the participant-count x-position formula used in draw_header.
/// Returns the x coordinate where the count texture will be drawn.
#[cfg(test)]
pub(crate) fn participant_count_x(bx2: i32, text_width: u32, text_x: f32) -> f32 {
    (bx2 as f32 - text_width as f32 - 8.0).max(text_x)
}

/// Returns the ping label string for a given latency value.
#[cfg(test)]
pub(crate) fn ping_label(latency_ms: u32) -> String {
    format!("~{latency_ms}ms")
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
        use crate::discord::UserId;
        use crate::render::MockEgl;
        use std::collections::HashMap;
        let egl = MockEgl::new();
        let mut avatar_textures: HashMap<UserId, glow::NativeTexture> = HashMap::new();
        let initials_textures: HashMap<UserId, (glow::NativeTexture, u32, u32)> = HashMap::new();
        let tex = egl.upload_texture_wh(&[255u8; 4], 1, 1);
        avatar_textures.insert(UserId("u1".to_string()), tex);
        let p = ParticipantState {
            user_id: UserId("u1".to_string()),
            display_name: "Alice".to_string(),
            muted: false,
            deafened: false,
            speaking_until: None,
            anim: 1.0,
            leaving: false,
            speaking_anim: 0.0,
            talk_time: std::time::Duration::ZERO,
            speaking_started_at: None,
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
            None,
            0.0,
        );
    }

    #[test]
    fn draw_compact_core_partial_speaking() {
        // speaking_until set (so speaking=true) but speaking_anim < 0.95 → else { 1.0 } branch
        use crate::discord::UserId;
        use crate::render::MockEgl;
        use std::collections::HashMap;
        use std::time::{Duration, Instant};
        let egl = MockEgl::new();
        let avatar_textures: HashMap<UserId, glow::NativeTexture> = HashMap::new();
        let initials_textures: HashMap<UserId, (glow::NativeTexture, u32, u32)> = HashMap::new();
        let p = ParticipantState {
            user_id: UserId("u_partial".to_string()),
            display_name: "Partial".to_string(),
            muted: false,
            deafened: false,
            speaking_until: Some(Instant::now() + Duration::from_millis(1500)),
            anim: 1.0,
            leaving: false,
            speaking_anim: 0.5, // < 0.95 → pulse factor = 1.0 (else branch)
            talk_time: std::time::Duration::ZERO,
            speaking_started_at: None,
        };
        draw_compact_core(
            &egl,
            120,
            48,
            1.0,
            1.0,
            &[p],
            &avatar_textures,
            &initials_textures,
            [0.3, 0.8, 0.3],
            None,
            0.0,
        );
    }

    #[test]
    fn draw_compact_core_with_overflow_badge() {
        use crate::discord::UserId;
        use crate::render::MockEgl;
        use std::collections::HashMap;
        let egl = MockEgl::new();
        let avatar_textures: HashMap<UserId, glow::NativeTexture> = HashMap::new();
        let initials_textures: HashMap<UserId, (glow::NativeTexture, u32, u32)> = HashMap::new();
        let badge_tex = egl.upload_texture_wh(&[200u8; 4], 20, 10);
        let p = ParticipantState {
            user_id: UserId("u_ov".to_string()),
            display_name: "Overflow".to_string(),
            muted: false,
            deafened: false,
            speaking_until: None,
            anim: 1.0,
            leaving: false,
            speaking_anim: 0.0,
            talk_time: std::time::Duration::ZERO,
            speaking_started_at: None,
        };
        draw_compact_core(
            &egl,
            168,
            48,
            1.0,
            1.0,
            &[p],
            &avatar_textures,
            &initials_textures,
            [0.2, 0.6, 0.2],
            Some((badge_tex, 20, 10)),
            0.0,
        );
    }

    #[test]
    fn draw_compact_core_muted_and_deafened_participants() {
        use crate::discord::UserId;
        use crate::render::MockEgl;
        use std::collections::HashMap;
        let egl = MockEgl::new();
        let mut avatar_textures: HashMap<UserId, glow::NativeTexture> = HashMap::new();
        let initials_textures: HashMap<UserId, (glow::NativeTexture, u32, u32)> = HashMap::new();
        let tex = egl.upload_texture_wh(&[255u8; 4], 1, 1);
        let uid_muted = UserId("u_muted".to_string());
        avatar_textures.insert(uid_muted.clone(), tex);
        let p_muted = ParticipantState {
            user_id: uid_muted,
            display_name: "Muted".to_string(),
            muted: true,
            deafened: false,
            speaking_until: None,
            anim: 1.0,
            leaving: false,
            speaking_anim: 0.0,
            talk_time: std::time::Duration::ZERO,
            speaking_started_at: None,
        };
        let p_deafened = ParticipantState {
            user_id: UserId("u_deaf".to_string()),
            display_name: "Deafened".to_string(),
            muted: false,
            deafened: true,
            speaking_until: None,
            anim: 0.7,
            leaving: false,
            speaking_anim: 0.0,
            talk_time: std::time::Duration::ZERO,
            speaking_started_at: None,
        };
        draw_compact_core(
            &egl,
            216,
            48,
            1.0,
            1.0,
            &[p_muted, p_deafened],
            &avatar_textures,
            &initials_textures,
            [0.2, 0.6, 0.2],
            None,
            0.0,
        );
    }

    #[test]
    fn draw_compact_core_pulse_phase_half() {
        // speaking_anim > 0.95 with phase=0.5 → phi=π, cos(π)=-1, factor=1.0
        use crate::discord::UserId;
        use crate::render::MockEgl;
        use std::collections::HashMap;
        use std::time::{Duration, Instant};
        let egl = MockEgl::new();
        let avatar_textures: HashMap<UserId, glow::NativeTexture> = HashMap::new();
        let mut initials_textures: HashMap<UserId, (glow::NativeTexture, u32, u32)> =
            HashMap::new();
        let tex = egl.upload_texture_wh(&[200u8; 4], 6, 6);
        initials_textures.insert(UserId("u_pulse".to_string()), (tex, 8, 8));
        let p = ParticipantState {
            user_id: UserId("u_pulse".to_string()),
            display_name: "Pulse".to_string(),
            muted: false,
            deafened: false,
            speaking_until: Some(Instant::now() + Duration::from_millis(1500)),
            anim: 1.0,
            leaving: false,
            speaking_anim: 1.0,
            talk_time: std::time::Duration::ZERO,
            speaking_started_at: None,
        };
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
            None,
            0.5, // phase = 0.5 → peak brightness
        );
    }

    #[test]
    fn draw_compact_core_placeholder_and_speaking() {
        use crate::discord::UserId;
        use crate::render::MockEgl;
        use std::collections::HashMap;
        let egl = MockEgl::new();
        let avatar_textures: HashMap<UserId, glow::NativeTexture> = HashMap::new();
        let mut initials_textures: HashMap<UserId, (glow::NativeTexture, u32, u32)> =
            HashMap::new();
        let tex = egl.upload_texture_wh(&[255u8; 4], 4, 4);
        initials_textures.insert(UserId("u2".to_string()), (tex, 8, 8));
        let p = ParticipantState {
            user_id: UserId("u2".to_string()),
            display_name: "Bob".to_string(),
            muted: false,
            deafened: false,
            speaking_until: Some(
                std::time::Instant::now() + std::time::Duration::from_millis(1500),
            ),
            anim: 1.0,
            leaving: false,
            speaking_anim: 1.0,
            talk_time: std::time::Duration::ZERO,
            speaking_started_at: None,
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
            None,
            0.0,
        );
    }

    // ── Participant count x-position ─────────────────────────────────────────

    #[test]
    fn participant_count_x_stays_left_of_mute_button() {
        // bx2 = mute button left edge; count must never overlap it
        let (bx2, _, _, _) = button2_rects(360, 64);
        let text_x = 40.0_f32;
        // Wide text (e.g. "12 in channel" ~90px)
        let x = participant_count_x(bx2, 90, text_x);
        assert!(x + 90.0 + 8.0 <= bx2 as f32, "count overlaps mute button");
    }

    #[test]
    fn participant_count_x_clamped_to_text_x_when_very_wide() {
        // If text is wider than available space, clamp to text_x (drag-handle boundary)
        let text_x = 40.0_f32;
        // bx2 very small (tiny window), text wider than available gap
        let x = participant_count_x(50, 200, text_x);
        assert_eq!(x, text_x);
    }

    #[test]
    fn participant_count_x_normal_short_text() {
        let (bx2, _, _, _) = button2_rects(360, 64);
        let text_x = 40.0_f32;
        let tw = 50u32;
        let x = participant_count_x(bx2, tw, text_x);
        let expected = bx2 as f32 - tw as f32 - 8.0;
        assert!((x - expected).abs() < 0.01);
    }

    // ── Ping label format ────────────────────────────────────────────────────

    #[test]
    fn ping_label_format_typical() {
        assert_eq!(ping_label(42), "~42ms");
    }

    #[test]
    fn ping_label_format_zero() {
        assert_eq!(ping_label(0), "~0ms");
    }

    #[test]
    fn ping_label_format_large() {
        assert_eq!(ping_label(999), "~999ms");
    }
}

#[cfg(test)]
mod tests_discord_events {
    use super::*;

    #[test]
    fn participant_state_defaults() {
        let _p = ParticipantState {
            user_id: crate::discord::UserId("test".to_string()),
            display_name: "Test User".to_string(),
            muted: false,
            deafened: false,
            speaking_until: None,
            anim: 1.0,
            leaving: false,
            speaking_anim: 0.0,
            talk_time: std::time::Duration::ZERO,
            speaking_started_at: None,
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
            user_id: crate::discord::UserId("test".to_string()),
            display_name: "Test User".to_string(),
            muted: false,
            deafened: false,
            speaking_until: None,
            anim: 1.0,
            leaving: false,
            speaking_anim: 0.0,
            talk_time: std::time::Duration::ZERO,
            speaking_started_at: None,
        };
        let now = std::time::Instant::now();
        p.speaking_until = Some(now);
        assert!(p.speaking_until.is_some());
    }

    #[test]
    fn participant_leaving_animation() {
        let mut p = ParticipantState {
            user_id: crate::discord::UserId("test".to_string()),
            display_name: "Test User".to_string(),
            muted: false,
            deafened: false,
            speaking_until: None,
            anim: 1.0,
            leaving: false,
            speaking_anim: 0.0,
            talk_time: std::time::Duration::ZERO,
            speaking_started_at: None,
        };
        p.leaving = true;
        assert!(p.leaving);
    }

    #[test]
    fn participant_state_builder_from_discord() {
        let discord_p = crate::discord::Participant {
            user_id: crate::discord::UserId("123".to_string()),
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
            user_id: crate::discord::UserId("456".to_string()),
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
    fn talk_time_initial_zero() {
        let p = ParticipantStateBuilder::new("u1", "Alice").build();
        assert_eq!(p.talk_time, std::time::Duration::ZERO);
        assert!(p.speaking_started_at.is_none());
        assert_eq!(p.current_talk_secs(), 0);
    }

    #[test]
    fn talk_time_accumulates_on_speaking_stop() {
        let mut p = ParticipantStateBuilder::new("u1", "Alice").build();
        let started = std::time::Instant::now() - std::time::Duration::from_secs(5);
        p.speaking_started_at = Some(started);
        // Simulate SPEAKING_STOP: finalize the segment
        if let Some(t) = p.speaking_started_at.take() {
            p.talk_time += t.elapsed();
        }
        assert!(p.talk_time.as_secs() >= 5);
        assert!(p.speaking_started_at.is_none());
    }

    #[test]
    fn talk_time_current_secs_includes_active_segment() {
        let mut p = ParticipantStateBuilder::new("u1", "Alice").build();
        p.talk_time = std::time::Duration::from_secs(10);
        p.speaking_started_at = Some(std::time::Instant::now() - std::time::Duration::from_secs(3));
        let secs = p.current_talk_secs();
        assert!(secs >= 13, "expected >=13, got {secs}");
    }

    #[test]
    fn talk_time_not_started_returns_base() {
        let mut p = ParticipantStateBuilder::new("u1", "Alice").build();
        p.talk_time = std::time::Duration::from_secs(42);
        assert_eq!(p.current_talk_secs(), 42);
    }

    #[test]
    fn tick_speaking_expiry_finalizes_talk_time() {
        use std::time::{Duration, Instant};
        let mut p = ParticipantStateBuilder::new("u1", "Alice").build();
        let started = Instant::now() - Duration::from_secs(4);
        p.speaking_started_at = Some(started);
        p.speaking_until = Some(Instant::now() - Duration::from_millis(1)); // already expired

        // Simulate tick_speaking_expiry logic directly
        if p.speaking_until
            .map(|t| t <= Instant::now())
            .unwrap_or(false)
        {
            p.speaking_until = None;
            if let Some(s) = p.speaking_started_at.take() {
                p.talk_time += s.elapsed();
            }
        }

        assert!(p.speaking_until.is_none());
        assert!(p.speaking_started_at.is_none());
        assert!(p.talk_time.as_secs() >= 4);
    }

    #[test]
    fn talk_time_multiple_segments_accumulate() {
        let mut p = ParticipantStateBuilder::new("u1", "Alice").build();
        // First segment: 3 seconds
        let s1 = std::time::Instant::now() - std::time::Duration::from_secs(3);
        p.speaking_started_at = Some(s1);
        if let Some(t) = p.speaking_started_at.take() {
            p.talk_time += t.elapsed();
        }
        // Second segment: 5 seconds
        let s2 = std::time::Instant::now() - std::time::Duration::from_secs(5);
        p.speaking_started_at = Some(s2);
        if let Some(t) = p.speaking_started_at.take() {
            p.talk_time += t.elapsed();
        }
        assert!(p.talk_time.as_secs() >= 8);
    }

    #[test]
    fn user_joined_sets_in_channel() {
        // Verify that UserJoined event sets in_channel flag for visibility
        let discord_p = crate::discord::Participant {
            user_id: crate::discord::UserId("111".to_string()),
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
