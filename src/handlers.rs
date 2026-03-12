use std::num::NonZeroU32;

use smithay_client_toolkit as sctk;
use sctk::compositor::{CompositorHandler, SurfaceData};
use sctk::output::{OutputHandler, OutputState};
use sctk::registry::{ProvidesRegistryState, RegistryState};
use sctk::reexports::client::protocol::{wl_output, wl_pointer, wl_seat};
use sctk::reexports::client::protocol::wl_keyboard::WlKeyboard;
use sctk::reexports::client::protocol::wl_surface::WlSurface;
use sctk::reexports::client::{Connection, Proxy, QueueHandle};
use sctk::seat::keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers, RepeatInfo};
use sctk::seat::pointer::{BTN_LEFT, PointerEvent, PointerEventKind, PointerHandler};
use sctk::seat::{Capability, SeatHandler, SeatState};
use sctk::shell::wlr_layer::{Anchor, LayerShellHandler, LayerSurface, LayerSurfaceConfigure};
use sctk::shell::WaylandSurface;
use sctk::{
    delegate_compositor, delegate_keyboard, delegate_layer, delegate_output, delegate_pointer,
    delegate_registry, delegate_seat, registry_handlers,
};

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

                    if !self.compact {
                        let (bx, by, bw, bh) = button_rects(self.width, 64);
                        if x >= bx && x < bx + bw && y >= by && y < by + bh {
                            // Deafen button
                            if let Some(ref tx) = self.discord_cmd_tx {
                                let _ = tx.send(discord::DiscordCommand::SetDeaf(!self.discord_deaf));
                            }
                        }
                        let (bx2, by2, bw2, bh2) = button2_rects(self.width, 64);
                        if x >= bx2 && x < bx2 + bw2 && y >= by2 && y < by2 + bh2 {
                            // Mute button
                            if let Some(ref tx) = self.discord_cmd_tx {
                                let _ = tx.send(discord::DiscordCommand::SetMute(!self.discord_mute));
                            }
                        }
                    }

                    // Determine if the click lands on the drag handle area.
                    // In compact mode the whole surface is the drag handle.
                    let on_drag_handle = if self.compact {
                        button == BTN_LEFT
                    } else {
                        let (hx, hy, hw, hh) = drag_handle_rects(self.width, 64);
                        button == BTN_LEFT && x >= hx && x < hx + hw && y >= hy && y < hy + hh
                    };

                    if on_drag_handle {
                        // Double-click within 400 ms toggles compact mode
                        let now = std::time::Instant::now();
                        let is_double_click = self
                            .last_click_time
                            .map(|t| t.elapsed() < std::time::Duration::from_millis(400))
                            .unwrap_or(false);
                        self.last_click_time = Some(now);

                        if is_double_click {
                            self.compact = !self.compact;
                            self.dragging = false; // cancel any in-progress drag
                            self.last_click_time = None;
                            self.apply_compact_resize();
                            self.clear_input_region();
                            self.draw();
                            return;
                        }

                        // Compute absolute surface position for drag reference
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
                                let surf_scale =
                                    self.layer
                                        .wl_surface()
                                        .data::<SurfaceData>()
                                        .map(|d| d.scale_factor())
                                        .unwrap_or(1) as i32;
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
                        self.dragging = true;
                        self.last_pointer = (x, y);
                        self.draw();
                    }
                }

                Release { button, .. } => {
                    if button == BTN_LEFT {
                        self.dragging = false;
                    }
                }
                Axis { vertical, .. } => {
                    let delta = -vertical.absolute as f32 * 0.02;
                    if self.last_pointer_y > 64.0 && !self.participants.is_empty() {
                        // Scroll participant list: wheel-down increases offset (show further down)
                        if delta < 0.0 {
                            let max_offset = self.participants.len().saturating_sub(self.max_visible_rows);
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
