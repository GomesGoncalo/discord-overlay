# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

**hypr-overlay-wl** is a Wayland-native Discord voice overlay for Hyprland. It renders on a `zwlr_layer_shell_v1` layer shell surface using EGL/GLES2, showing voice channel participants with speaking indicators and mute/deafen controls.

## Commands

```bash
# Build
cargo build --release

# Run all tests (single-threaded — IPC socket is global)
RUST_TEST_THREADS=1 cargo test --verbose

# Run a single test
RUST_TEST_THREADS=1 cargo test <test_name> -- --test-threads=1 --nocapture

# Lint (must pass with no warnings)
cargo clippy --all-targets --all-features -- -D warnings

# Format
cargo fmt

# Install/uninstall (binary + systemd service)
make install
make uninstall
```

All CI checks: build → clippy → fmt check → test → `bash scripts/check-coverage.sh` (tarpaulin, baseline 34.08%).

## Architecture

Event-driven via **calloop**. `main.rs` creates an `App` struct and runs an event loop with four sources:

1. **Wayland events** → `handlers.rs` (pointer/keyboard input, hit detection, drag)
2. **Discord IPC channel** (background thread) → `state/mod.rs::handle_discord_event()`
3. **Animation timer** (16 ms when animating, 500 ms idle) → `state/mod.rs::draw()`
4. **Config file watcher** (inotify) → `config.rs` hot-reload

### Module Responsibilities

| Module | Role |
|--------|------|
| `state/mod.rs` | Core `App` struct, participant animations, `draw()` (the main render entrypoint) |
| `state/participant.rs` | `ParticipantState` with join/leave animation (`anim: f32`, `leaving: bool`) |
| `discord/client.rs` | IPC client loop, reconnect with exponential backoff, OAuth2 flow |
| `discord/handlers.rs` | Discord RPC event type dispatch → `DiscordEvent` variants |
| `discord/types.rs` | `UserId`, `Participant`, `DiscordEvent`, `DiscordCommand` |
| `discord/auth.rs` | OAuth2 browser flow, token refresh, disk caching |
| `discord/ipc.rs` | Unix socket I/O, frame encode/decode |
| `discord/parser.rs` | Discord JSON → `Participant` struct |
| `render/egl_context.rs` | EGL/GLES2 context setup, GL resource ownership (RAII via `Drop`) |
| `render/program.rs` | Generic shader program wrappers (`EglBackend` trait enables testing without GPU) |
| `render/shaders.rs` | GLSL source (GLES2 `#version 100`): rounded-rect SDF, texture/avatar/icon frags |
| `render/compile.rs` | Shader compilation and error reporting |
| `render/draw.rs` | Draw call issuance (geometry + uniforms) |
| `render/text.rs` | Fontdue rasterization → GL textures |
| `handlers.rs` | Wayland pointer/keyboard handlers, `button_rects()`, `drag_handle_rects()` |
| `config.rs` | TOML parsing from `~/.config/hypr-overlay/config.toml`, env var overrides |
| `avatar.rs` | Background PNG download from Discord CDN → RGBA8 → GL texture |

### Key Types

- **`App`** (`state/mod.rs`): Holds all state — Wayland handles, participants list, avatar/name textures (`HashMap<UserId, ...>`), opacity, scroll offset, compact mode, PTT mode, config.
- **`ParticipantState`** (`state/participant.rs`): Per-user state including `speaking_until: Option<Instant>` (1.5s timeout), `anim: f32` (0→1 fade), `leaving: bool`.
- **`DiscordEvent`**: Enum dispatched from the background IPC thread to the main calloop via a channel.

### Rendering

Four GLSL fragment shaders handle: rounded-rect SDF (buttons/drag handle), icon textures (mute/deafen), avatar textures (circular clip + optional greyscale for deafened), and text. The `EglBackend` trait abstracts GPU calls to enable unit testing without a display.

### Testing Notes

Tests must run single-threaded (`RUST_TEST_THREADS=1`) because the Discord IPC socket path is process-global. Use `#[serial_test::serial]` for tests that touch socket state. The `EglBackend` trait allows testing render logic without a real GPU context.

## Environment Variables

- `DISCORD_CLIENT_ID`, `DISCORD_CLIENT_SECRET` — OAuth2 credentials
- `OVERLAY_OPACITY` — Override config opacity
- `RUST_LOG` — Tracing log level (e.g., `debug`, `hypr_overlay_wl=trace`)
