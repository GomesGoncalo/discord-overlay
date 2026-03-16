use std::num::NonZeroU32;

use sctk::compositor::{CompositorHandler, SurfaceData};
use sctk::output::{OutputHandler, OutputState};
use sctk::reexports::client::protocol::wl_keyboard::WlKeyboard;
use sctk::reexports::client::protocol::wl_surface::WlSurface;
use sctk::reexports::client::protocol::{wl_output, wl_pointer, wl_seat};
use sctk::reexports::client::{Connection, Proxy, QueueHandle};
use sctk::registry::{ProvidesRegistryState, RegistryState};
use sctk::seat::keyboard::{
    KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers, RepeatInfo,
};
use sctk::seat::pointer::{PointerEvent, PointerEventKind, PointerHandler, BTN_LEFT};
use sctk::seat::{Capability, SeatHandler, SeatState};
use sctk::shell::wlr_layer::{Anchor, LayerShellHandler, LayerSurface, LayerSurfaceConfigure};
use sctk::shell::WaylandSurface;
use sctk::{
    delegate_compositor, delegate_keyboard, delegate_layer, delegate_output, delegate_pointer,
    delegate_registry, delegate_seat, registry_handlers,
};
use smithay_client_toolkit as sctk;

use tracing::{debug, trace};

use crate::discord;
use crate::state::App;

// ─── Layout helpers ──────────────────────────────────────────────────────────

pub fn button_rects(w: u32, h: u32) -> (i32, i32, i32, i32) {
    let bw = 64;
    let bh = (h as i32 - 16).max(1);
    (w as i32 - bw - 8, 8, bw, bh)
}
pub fn button2_rects(w: u32, h: u32) -> (i32, i32, i32, i32) {
    let bw = 64;
    let bh = (h as i32 - 16).max(1);
    (w as i32 - bw - 8 - bw - 8, 8, bw, bh)
}
pub fn drag_handle_rects(_w: u32, h: u32) -> (i32, i32, i32, i32) {
    (8, 8, 24, (h as i32 - 16).max(1))
}

// ─── Trait implementations ───────────────────────────────────────────────────

impl CompositorHandler for App {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlSurface,
        _: i32,
    ) {
    }
    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlSurface,
        _: wl_output::Transform,
    ) {
    }
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlSurface, _: u32) {}
    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl LayerShellHandler for App {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        self.exit = true;
    }
    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _: u32,
    ) {
        let (w, h) = configure.new_size;
        if w != 0 {
            self.width = NonZeroU32::new(w).map_or(self.width, |v| v.get());
        }
        if h != 0 {
            self.height = NonZeroU32::new(h).map_or(self.height, |v| v.get());
        }
        self.egl.resize(self.width as i32, self.height as i32);
        self.draw();
    }
}

impl SeatHandler for App {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Pointer {
            let _ = self.seat_state.get_pointer(qh, &seat);
        }
        if capability == Capability::Keyboard {
            let _ = self.seat_state.get_keyboard::<_, App>(qh, &seat, None);
        }
    }
    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        _: Capability,
    ) {
    }
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl PointerHandler for App {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        use PointerEventKind::*;
        for event in events {
            if event.surface.id() != self.layer.wl_surface().id() {
                continue;
            }
            match event.kind {
                Enter { .. } | Leave { .. } => {
                    if let Enter { .. } = event.kind {
                        self.last_pointer_y = event.position.1;
                    }
                }

                Motion { .. } => {
                    self.last_pointer_y = event.position.1;
                    if self.dragging {
                        let (x, y) = (event.position.0 as i32, event.position.1 as i32);

                        // Reconstruct absolute pointer position from expected surface pos +
                        // surface-local event coordinates, then subtract grab offset.
                        let ptr_screen_x = self.drag_base_pos.0 + x;
                        let ptr_screen_y = self.drag_base_pos.1 + y;
                        let new_x = ptr_screen_x - self.last_pointer.0;
                        let new_y = ptr_screen_y - self.last_pointer.1;
                        self.drag_base_pos = (new_x, new_y);

                        if let Some(out) = self.drag_output.clone() {
                            if let Some(info) = self.output_state.info(&out) {
                                let out_pos = info.logical_position.unwrap_or((0, 0));
                                let out_scale = info.scale_factor.max(1);
                                let left = (new_x - out_pos.0 * out_scale) / out_scale;
                                let top = (new_y - out_pos.1 * out_scale) / out_scale;
                                self.layer.set_anchor(Anchor::TOP | Anchor::LEFT);
                                self.layer.set_margin(top, 0, 0, left);
                                self.margins = (top, 0, 0, left); // keep in sync with drag_base_pos
                                self.draw(); // egl.swap() commits margin changes + buffer
                            }
                        }
                    }
                }

                Press { button, .. } => {
                    let (x, y) = (event.position.0 as i32, event.position.1 as i32);

                    if !self.compact && self.handle_button_clicks(x, y) {
                        self.draw(); // immediately show pressed state
                    }

                    // In compact mode the whole surface is the drag handle.
                    let on_drag_handle = if self.compact {
                        button == BTN_LEFT
                    } else {
                        let (hx, hy, hw, hh) = drag_handle_rects(self.width, 64);
                        button == BTN_LEFT && x >= hx && x < hx + hw && y >= hy && y < hy + hh
                    };

                    if on_drag_handle {
                        // Double-click within 400 ms toggles compact mode
                        let is_double_click = self
                            .last_click_time
                            .map(|t| t.elapsed() < std::time::Duration::from_millis(400))
                            .unwrap_or(false);
                        self.last_click_time = Some(std::time::Instant::now());

                        if is_double_click {
                            self.compact = !self.compact;
                            debug!(compact = self.compact, "compact mode toggled");
                            self.dragging = false;
                            self.last_click_time = None;
                            self.apply_compact_resize();
                            self.clear_input_region();
                            self.draw();
                            return;
                        }

                        self.start_drag(x, y);
                    }
                }

                Release { button, .. } => {
                    if button == BTN_LEFT {
                        if self.dragging {
                            debug!("drag released");
                        }
                        self.dragging = false;
                        if self.mute_held || self.deaf_held {
                            self.mute_held = false;
                            self.deaf_held = false;
                            self.draw(); // restore normal button colors
                        }
                    }
                }
                Axis { vertical, .. } => {
                    let delta = -vertical.absolute as f32 * 0.02;
                    trace!(delta, pointer_y = self.last_pointer_y, "scroll event");
                    if self.last_pointer_y > 64.0 && !self.participants.is_empty() {
                        // Scroll participant list: wheel-down increases offset (show further down)
                        if delta < 0.0 {
                            let max_offset = self
                                .participants
                                .len()
                                .saturating_sub(self.max_visible_rows);
                            self.scroll_offset = (self.scroll_offset + 1).min(max_offset);
                        } else {
                            self.scroll_offset = self.scroll_offset.saturating_sub(1);
                        }
                    } else {
                        // Control bar: adjust overlay opacity
                        self.opacity = (self.opacity + delta).clamp(0.1, 1.0);
                    }
                    self.draw();
                }
            }
        }
    }
}

impl KeyboardHandler for App {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlKeyboard,
        _: &WlSurface,
        _: u32,
        _: &[u32],
        _: &[Keysym],
    ) {
    }
    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlKeyboard,
        _: &WlSurface,
        _: u32,
    ) {
    }
    fn press_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlKeyboard,
        _: u32,
        _: KeyEvent,
    ) {
    }
    fn repeat_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlKeyboard,
        _: u32,
        _: KeyEvent,
    ) {
    }
    fn release_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlKeyboard,
        _: u32,
        _: KeyEvent,
    ) {
    }
    fn update_modifiers(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlKeyboard,
        _: u32,
        modifiers: Modifiers,
        _: RawModifiers,
        _: u32,
    ) {
        self.modifiers = modifiers;
    }
    fn update_repeat_info(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlKeyboard,
        _: RepeatInfo,
    ) {
    }
}

// ─── Pointer helper methods ───────────────────────────────────────────────────

impl App {
    /// Handle clicks on the mute/deafen buttons (non-compact mode only).
    /// Returns true if any button was hit (for immediate visual feedback).
    fn handle_button_clicks(&mut self, x: i32, y: i32) -> bool {
        let mut clicked = false;
        let (bx, by, bw, bh) = button_rects(self.width, 64);
        if x >= bx && x < bx + bw && y >= by && y < by + bh {
            self.deaf_held = true;
            clicked = true;
            let new_deaf = !self.discord_deaf;
            debug!(deafen = new_deaf, "deafen button clicked");
            if let Some(ref tx) = self.discord_cmd_tx {
                let _ = tx.send(discord::DiscordCommand::SetDeaf(new_deaf));
            }
        }
        let (bx2, by2, bw2, bh2) = button2_rects(self.width, 64);
        if x >= bx2 && x < bx2 + bw2 && y >= by2 && y < by2 + bh2 {
            self.mute_held = true;
            clicked = true;
            let new_mute = !self.discord_mute;
            debug!(mute = new_mute, "mute button clicked");
            if let Some(ref tx) = self.discord_cmd_tx {
                let _ = tx.send(discord::DiscordCommand::SetMute(new_mute));
            }
        }
        clicked
    }

    /// Convert the current anchor/margin state to an absolute screen position and
    /// begin a drag. `(x, y)` are the surface-local pointer coordinates at press time.
    fn start_drag(&mut self, x: i32, y: i32) {
        let outputs: Vec<_> = self
            .layer
            .wl_surface()
            .data::<SurfaceData>()
            .map(|d| d.outputs().collect())
            .unwrap_or_default();
        if let Some(output) = outputs.first() {
            let out = output.clone();
            if let Some(info) = self.output_state.info(&out) {
                let out_pos = info.logical_position.unwrap_or((0, 0));
                let out_size = info.logical_size.unwrap_or((1920, 1080));
                let out_scale = info.scale_factor;

                let (top, right, bottom, left) = self.margins;
                let surf_scale = self
                    .layer
                    .wl_surface()
                    .data::<SurfaceData>()
                    .map(|d| d.scale_factor())
                    .unwrap_or(1);
                let surf_w = self.width as i32 * surf_scale;
                let surf_h = self.height as i32 * surf_scale;

                let out_pos_px = (out_pos.0 * out_scale, out_pos.1 * out_scale);
                let out_size_px = (out_size.0 * out_scale, out_size.1 * out_scale);

                let abs_left = if self.anchor.contains(Anchor::RIGHT) {
                    out_pos_px.0 + out_size_px.0 - right - surf_w
                } else {
                    out_pos_px.0 + left
                };
                let abs_top = if self.anchor.contains(Anchor::BOTTOM) {
                    out_pos_px.1 + out_size_px.1 - bottom - surf_h
                } else {
                    out_pos_px.1 + top
                };

                self.anchor = Anchor::TOP | Anchor::LEFT;
                self.layer.set_anchor(self.anchor);
                let top_m = (abs_top - out_pos_px.1) / out_scale;
                let left_m = (abs_left - out_pos_px.0) / out_scale;
                self.layer.set_margin(top_m, 0, 0, left_m);
                self.drag_base_pos = (abs_left, abs_top);
                self.drag_output = Some(out);
            }
        }
        debug!(x, y, "drag started");
        self.dragging = true;
        self.last_pointer = (x, y);
        self.draw();
    }
}

// ─── Delegate macros ─────────────────────────────────────────────────────────

delegate_compositor!(App);
delegate_output!(App);
delegate_seat!(App);
delegate_pointer!(App);
delegate_layer!(App);
delegate_registry!(App);

delegate_keyboard!(App);

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn button_rects_basic() {
        let (x, y, w, h) = button_rects(360, 64);
        assert_eq!(w, 64);
        assert_eq!(h, 48);
        assert_eq!(x, 360i32 - w - 8);
        assert_eq!(y, 8);
    }

    #[test]
    fn button2_rects_basic() {
        let (x, _y, w, h) = button2_rects(360, 64);
        assert_eq!(w, 64);
        assert_eq!(h, 48);
        assert_eq!(x, 360i32 - w - 8 - w - 8);
    }

    #[test]
    fn drag_handle() {
        let (x, y, w, h) = drag_handle_rects(360, 64);
        assert_eq!(x, 8);
        assert_eq!(y, 8);
        assert_eq!(w, 24);
        assert!(h > 0);
    }

    #[test]
    fn small_height_button_rects() {
        let (_x, _y, _w, h) = button_rects(100, 10);
        assert_eq!(h, 1);
        let (_x2, _y2, _w2, h2) = button2_rects(100, 10);
        assert_eq!(h2, 1);
        let (_hx, _hy, _hw, hh) = drag_handle_rects(100, 10);
        assert_eq!(hh, 1);
    }

    #[test]
    fn button_rects_preserves_width() {
        let (_, _, w, _) = button_rects(200, 100);
        assert_eq!(w, 64);

        let (_, _, w, _) = button_rects(500, 50);
        assert_eq!(w, 64);
    }

    #[test]
    fn button2_rects_at_right_edge() {
        let (x1, _, _w1, _) = button_rects(360, 64);
        let (x2, _, w2, _) = button2_rects(360, 64);
        assert_eq!(x2, x1 - w2 - 8);
    }

    #[test]
    fn drag_handle_ignores_width() {
        let (x, _, _, _) = drag_handle_rects(200, 64);
        assert_eq!(x, 8);

        let (x, _, _, _) = drag_handle_rects(800, 100);
        assert_eq!(x, 8);
    }

    #[test]
    fn button_height_scales_with_window() {
        let (_, _, _, h) = button_rects(100, 100);
        assert_eq!(h, 84);

        let (_, _, _, h) = button_rects(100, 50);
        assert_eq!(h, 34);

        let (_, _, _, h) = button_rects(100, 20);
        assert_eq!(h, 4);

        let (_, _, _, h) = button_rects(100, 8);
        assert_eq!(h, 1);
    }

    #[test]
    fn button_y_always_8() {
        let (_, y, _, _) = button_rects(100, 64);
        assert_eq!(y, 8);

        let (_, y, _, _) = button_rects(500, 200);
        assert_eq!(y, 8);
    }
}
