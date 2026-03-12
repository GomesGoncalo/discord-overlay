use std::collections::HashMap;
use std::sync::mpsc;

use smithay_client_toolkit as sctk;
use sctk::compositor::{CompositorState, Region};
use sctk::output::OutputState;
use sctk::registry::RegistryState;
use sctk::seat::keyboard::Modifiers;
use sctk::seat::SeatState;
use sctk::shell::wlr_layer::{Anchor, LayerSurface};
use sctk::shell::WaylandSurface;
use sctk::reexports::client::protocol::wl_output;

use glow::HasContext;

use crate::render::{render_text_texture, upload_texture_wh, EglContext};
use crate::discord;
use crate::handlers::{button_rects, button2_rects, drag_handle_rects};

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

// ─── App ─────────────────────────────────────────────────────────────────────

pub struct App {
    pub registry_state: RegistryState,
    pub seat_state: SeatState,
    pub output_state: OutputState,
    pub compositor: CompositorState,
    pub layer: LayerSurface,
    pub egl: EglContext,
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
    pub font: Option<fontdue::Font>,
    // Channel name display
    pub channel_name: Option<String>,
    pub channel_name_tex: Option<(glow::NativeTexture, u32, u32)>,
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
}

// ─── App methods ─────────────────────────────────────────────────────────────

impl App {
    pub fn visible_row_count(&self) -> usize {
        self.participants.len().min(self.max_visible_rows)
    }

    /// Rasterise `text` at `px_size` and upload it as a GL texture.
    pub fn render_text_tex(&self, text: &str, px_size: f32) -> Option<(glow::NativeTexture, u32, u32)> {
        let font = self.font.as_ref()?;
        let (pixels, w, h) = render_text_texture(font, text, px_size);
        if w > 0 && h > 0 {
            let tex = unsafe { upload_texture_wh(&self.egl.gl, &pixels, w, h) };
            Some((tex, w, h))
        } else {
            None
        }
    }

    pub fn resize_overlay(&mut self, new_h: u32) {
        if self.height == new_h {
            return;
        }
        eprintln!(
            "[overlay] resize {} → {} px tall ({} participant rows)",
            self.height,
            new_h,
            self.participants.len()
        );
        self.height = new_h;
        self.egl.resize(self.width as i32, new_h as i32);
        self.layer.set_size(self.width, new_h);
    }

    pub fn make_name_texture(&mut self, user_id: &str, name: &str) {
        if let Some(font) = &self.font {
            let (pixels, w, h) = render_text_texture(font, name, 16.0);
            if w > 0 && h > 0 {
                let tex = unsafe { upload_texture_wh(&self.egl.gl, &pixels, w, h) };
                self.name_textures.insert(user_id.to_string(), (tex, w, h));
            }
        }
    }

    pub fn handle_discord_event(&mut self, event: discord::DiscordEvent) -> bool {
        match event {
            discord::DiscordEvent::Ready { username } => {
                println!("Discord connected as {username}");
                false
            }
            discord::DiscordEvent::VoiceSettings { mute, deaf } => {
                let changed = self.discord_mute != mute || self.discord_deaf != deaf;
                self.discord_mute = mute;
                self.discord_deaf = deaf;
                changed
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
                    if let Some((tex, _, _)) = self.timer_tex.take() {
                        unsafe { self.egl.gl.delete_texture(tex); }
                    }
                }
                if self.channel_name != channel_name {
                    if let Some((tex, _, _)) = self.channel_name_tex.take() {
                        unsafe {
                            self.egl.gl.delete_texture(tex);
                        }
                    }
                    self.channel_name = channel_name.clone();
                    if let Some(ref name) = channel_name {
                        if let Some(font) = &self.font {
                            let (pixels, w, h) = render_text_texture(font, name, 14.0);
                            if w > 0 && h > 0 {
                                let tex = unsafe { upload_texture_wh(&self.egl.gl, &pixels, w, h) };
                                self.channel_name_tex = Some((tex, w, h));
                            }
                        }
                    }
                }
                for (_, (tex, _, _)) in self.name_textures.drain() {
                    unsafe {
                        self.egl.gl.delete_texture(tex);
                    }
                }
                self.participants = parts
                    .iter()
                    .map(|p| ParticipantState {
                        user_id: p.user_id.clone(),
                        display_name: p.nick.clone().unwrap_or_else(|| p.username.clone()),
                        muted: p.muted,
                        deafened: p.deafened,
                        speaking_until: None,
                        anim: 1.0, // already visible on initial load
                        leaving: false,
                    })
                    .collect();
                let user_names: Vec<(String, String)> = self
                    .participants
                    .iter()
                    .map(|p| (p.user_id.clone(), p.display_name.clone()))
                    .collect();
                for (uid, name) in user_names {
                    self.make_name_texture(&uid, &name);
                }
                // Reset scroll when participant list is fully replaced
                self.scroll_offset = 0;
                if let Some((tex, _, _)) = self.scroll_indicator_tex.take() {
                    unsafe { self.egl.gl.delete_texture(tex); }
                }
                self.last_scroll_state = (usize::MAX, usize::MAX);
                let extra = if self.participants.len() > self.max_visible_rows { 20 } else { 0 };
                let new_h = 64 + self.visible_row_count() as u32 * 48 + extra;
                self.resize_overlay(new_h);
                true
            }
            discord::DiscordEvent::UserJoined(p) => {
                // Ignore if already in list (e.g. duplicate event)
                if self.participants.iter().any(|e| e.user_id == p.user_id) {
                    return false;
                }
                eprintln!(
                    "[overlay] {} joined the channel",
                    p.nick.as_deref().unwrap_or(&p.username)
                );
                let uid = p.user_id.clone();
                let name = p.nick.clone().unwrap_or_else(|| p.username.clone());
                self.participants.push(ParticipantState {
                    user_id: uid.clone(),
                    display_name: name.clone(),
                    muted: p.muted,
                    deafened: p.deafened,
                    speaking_until: None,
                    anim: 0.0, // start invisible, animate in
                    leaving: false,
                });
                self.make_name_texture(&uid, &name);
                let extra = if self.participants.len() > self.max_visible_rows { 20 } else { 0 };
                let new_h = 64 + self.visible_row_count() as u32 * 48 + extra;
                self.resize_overlay(new_h);
                true
            }
            discord::DiscordEvent::UserLeft { user_id } => {
                if let Some(p) = self
                    .participants
                    .iter_mut()
                    .find(|p| p.user_id == user_id && !p.leaving)
                {
                    eprintln!(
                        "[overlay] {} leaving channel (animating out)",
                        p.display_name
                    );
                    p.leaving = true;
                    return true; // trigger a redraw to start the animation
                }
                false
            }
            discord::DiscordEvent::ParticipantStateUpdate {
                user_id,
                muted,
                deafened,
            } => {
                if let Some(p) = self.participants.iter_mut().find(|p| p.user_id == user_id) {
                    let changed = p.muted != muted || p.deafened != deafened;
                    p.muted = muted;
                    p.deafened = deafened;
                    return changed;
                }
                false
            }
            discord::DiscordEvent::SpeakingUpdate { user_id, speaking } => {
                if let Some(p) = self.participants.iter_mut().find(|p| p.user_id == user_id) {
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
                let tex = unsafe { upload_texture_wh(&self.egl.gl, &rgba, size, size) };
                self.avatar_textures.insert(user_id, tex);
                true
            }
            discord::DiscordEvent::Disconnected => {
                // Clear all voice-channel state so the UI resets to idle.
                self.in_channel = false;
                self.channel_name = None;
                if let Some((tex, _, _)) = self.channel_name_tex.take() {
                    unsafe {
                        self.egl.gl.delete_texture(tex);
                    }
                }
                // Clear session timer
                self.channel_joined_at = None;
                if let Some((tex, _, _)) = self.timer_tex.take() {
                    unsafe { self.egl.gl.delete_texture(tex); }
                }
                // Clear scroll indicator
                if let Some((tex, _, _)) = self.scroll_indicator_tex.take() {
                    unsafe { self.egl.gl.delete_texture(tex); }
                }
                self.scroll_offset = 0;
                self.last_scroll_state = (usize::MAX, usize::MAX);
                for (_, tex) in self.avatar_textures.drain() {
                    unsafe {
                        self.egl.gl.delete_texture(tex);
                    }
                }
                for (_, (tex, _, _)) in self.name_textures.drain() {
                    unsafe {
                        self.egl.gl.delete_texture(tex);
                    }
                }
                self.participants.clear();
                self.discord_mute = false;
                self.discord_deaf = false;
                self.idle_alpha = 0.3;
                self.resize_overlay(64);
                true
            }
        }
    }

    pub fn draw(&mut self) {
        let op = self.opacity * self.idle_alpha;
        let (sw, sh) = (self.width as f32, self.height as f32);
        let (bx, by, bw, bh) = button_rects(self.width, 64);
        let (bx2, by2, bw2, bh2) = button2_rects(self.width, 64);
        let (hx, hy, hw, hh) = drag_handle_rects(self.width, 64);

        unsafe {
            self.egl
                .gl
                .viewport(0, 0, self.width as i32, self.height as i32);
            self.egl.gl.clear_color(0.0, 0.0, 0.0, 0.0);
            self.egl.gl.clear(glow::COLOR_BUFFER_BIT);
            self.egl.gl.use_program(Some(self.egl.program));
        }

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

        // Channel name — rendered between drag handle and mute button
        // When a timer is also shown, push name to upper half of the bar.
        if let Some((tex, tw, th)) = self.channel_name_tex {
            let name_x = (hx + hw) as f32 + 8.0;
            let name_y = if self.timer_tex.is_some() {
                8.0
            } else {
                (64.0 - th as f32) * 0.5
            };
            let max_w = (bx2 as f32 - name_x - 8.0).max(0.0);
            let draw_w = (tw as f32).min(max_w);
            if draw_w > 0.0 {
                self.egl
                    .draw_icon(name_x, name_y, draw_w, th as f32, sw, sh, tex, op);
            }
        }

        // Session duration timer
        if let Some((tex, tw, th)) = self.timer_tex {
            let name_x = (hx + hw) as f32 + 8.0;
            let max_w = (bx2 as f32 - name_x - 8.0).max(0.0);
            let draw_w = (tw as f32).min(max_w);
            let timer_y = if self.channel_name_tex.is_some() {
                8.0 + 22.0 // below channel name (channel name is ~20px tall at 14px font)
            } else {
                (64.0 - th as f32) * 0.5
            };
            if draw_w > 0.0 {
                self.egl
                    .draw_icon(name_x, timer_y, draw_w, th as f32, sw, sh, tex, op * 0.7);
            }
        }

        // Mute button background
        // When deafened, mic is implicitly muted too
        let effectively_muted = self.discord_mute || self.discord_deaf;
        let mute_base = if effectively_muted {
            [0.75, 0.15, 0.15, 0.88 * op]
        } else {
            [0.12, 0.68, 0.28, 0.82 * op]
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
        let mic_tex = self.egl.tex_mic;
        let hp_tex = self.egl.tex_headphone;
        let strike_tex = self.egl.tex_strikeout;
        self.egl.draw_icon(
            bx2 as f32 + pad,
            by2 as f32 + pad,
            bw2 as f32 - 2.0 * pad,
            bh2 as f32 - 2.0 * pad,
            sw,
            sh,
            mic_tex,
            op,
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
                if let Some((tex, _, _)) = self.scroll_indicator_tex.take() {
                    unsafe { self.egl.gl.delete_texture(tex); }
                }
                let above = self.scroll_offset;
                let below = self.participants.len()
                    .saturating_sub(self.scroll_offset + self.max_visible_rows);
                let label = match (above > 0, below > 0) {
                    (true, true)  => format!("↑{}  ↓{}", above, below),
                    (true, false) => format!("↑{} more above", above),
                    (false, true) => format!("↓{} more below", below),
                    _             => String::new(),
                };
                if !label.is_empty() {
                    let new_tex = self.render_text_tex(&label, 11.0);
                    self.scroll_indicator_tex = new_tex;
                }
            }
        } else if self.scroll_indicator_tex.is_some() {
            if let Some((tex, _, _)) = self.scroll_indicator_tex.take() {
                unsafe { self.egl.gl.delete_texture(tex); }
            }
            self.last_scroll_state = (usize::MAX, usize::MAX);
        }

        // Collect visible participant data to avoid re-borrowing self in the loop
        let visible: Vec<(usize, f32, bool, bool, bool, String)> = self
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
                (abs_idx, p.anim, p.muted, p.deafened, speaking, p.user_id.clone())
            })
            .collect();

        for (slot, (abs_idx, anim, muted, deafened, speaking, user_id)) in
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
                [0.1, 0.1, 0.12, 0.6 * row_anim_op],
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
                    [0.2, 0.85, 0.35, 0.9 * row_anim_op],
                    (av_size + ring * 2.0) * 0.5,
                );
            }

            // Avatar
            if let Some(&tex) = self.avatar_textures.get(user_id) {
                self.egl
                    .draw_avatar(av_x, av_y, av_size, sw, sh, tex, row_anim_op);
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
            let mic_tex = self.egl.tex_mic;
            let hp_tex = self.egl.tex_headphone;
            let strike_tex = self.egl.tex_strikeout;
            let icon_y = row_y_f + (row_h as f32 - icon_sz) * 0.5;
            let mic_x = sw - icon_sz * 2.0 - icon_gap - 8.0;
            let hp_x = sw - icon_sz - 8.0;
            let mic_op = if *muted { row_anim_op * 0.9 } else { row_anim_op * 0.35 };
            if *muted {
                self.egl.draw_rect(
                    mic_x - 2.0,
                    icon_y - 2.0,
                    icon_sz + 4.0,
                    icon_sz + 4.0,
                    sw,
                    sh,
                    [0.7, 0.15, 0.15, 0.6 * row_anim_op],
                    4.0,
                );
            }
            self.egl
                .draw_icon(mic_x, icon_y, icon_sz, icon_sz, sw, sh, mic_tex, mic_op);
            if *muted {
                self.egl.draw_icon(
                    mic_x, icon_y, icon_sz, icon_sz, sw, sh, strike_tex,
                    row_anim_op * 0.85,
                );
            }
            let hp_op = if *deafened { row_anim_op * 0.9 } else { row_anim_op * 0.35 };
            if *deafened {
                self.egl.draw_rect(
                    hp_x - 2.0,
                    icon_y - 2.0,
                    icon_sz + 4.0,
                    icon_sz + 4.0,
                    sw,
                    sh,
                    [0.7, 0.15, 0.15, 0.6 * row_anim_op],
                    4.0,
                );
            }
            self.egl
                .draw_icon(hp_x, icon_y, icon_sz, icon_sz, sw, sh, hp_tex, hp_op);
            if *deafened {
                self.egl.draw_icon(
                    hp_x, icon_y, icon_sz, icon_sz, sw, sh, strike_tex,
                    row_anim_op * 0.85,
                );
            }
        }

        // Scroll indicator strip (only when there are more participants than visible rows)
        if self.participants.len() > self.max_visible_rows {
            let indicator_y = 64.0 + self.visible_row_count() as f32 * row_h as f32;
            self.egl.draw_rect(
                0.0, indicator_y, sw, 20.0, sw, sh,
                [0.15, 0.15, 0.18, op * 0.9],
                0.0,
            );
            if let Some((tex, tw, th)) = self.scroll_indicator_tex {
                let tx = (sw - tw as f32) * 0.5;
                let ty = indicator_y + (20.0 - th as f32) * 0.5;
                self.egl.draw_icon(tx, ty, tw as f32, th as f32, sw, sh, tex, op * 0.7);
            }
        }

        self.egl.swap();
    }
}
