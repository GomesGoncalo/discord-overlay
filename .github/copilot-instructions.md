# Copilot Instructions for discord-overlay

## Build, Test & Lint

### Build
```bash
cargo build --release
```

### Run tests
Run all tests (single-threaded due to IPC socket sharing):
```bash
RUST_TEST_THREADS=1 cargo test --verbose
```

Run a single test by name:
```bash
RUST_TEST_THREADS=1 cargo test <test_name> -- --test-threads=1 --nocapture
```

### Format and lint
```bash
cargo fmt
cargo clippy --all-targets --all-features -- -D warnings
```

### Check coverage baseline
Coverage must not decrease. Current baseline: 34.08%. Run:
```bash
bash scripts/check-coverage.sh
```

If you improve coverage, update the baseline:
```bash
echo $(jq '.coverage' tarpaulin-report.json | xargs printf '%.2f') > coverage-baseline.txt
```

All CI checks (pre-commit):
```bash
cargo build --verbose && \
cargo clippy --all-targets --all-features -- -D warnings && \
cargo fmt -- --check && \
RUST_TEST_THREADS=1 cargo test --verbose && \
bash scripts/check-coverage.sh
```

## Architecture

### Stack
- **Wayland protocol**: `smithay-client-toolkit` (sctk) — wraps libwayland-client
- **Event loop**: `calloop` with `calloop-wayland-source` — async reactor pattern
- **Rendering**: EGL + GLES2 via `glow` — hardware-accelerated on Mesa
- **Discord RPC**: Unix socket IPC with JSON payloads; OAuth2 token caching
- **Text rendering**: `fontdue` (signed distance field) → GL textures
- **Logging**: `tracing` macros with `tracing-subscriber` ENV filtering (RUST_LOG)

### Module responsibilities

| File | Role |
|------|------|
| `src/main.rs` | Initializes Wayland registry, event loop, surface, and runs calloop reactor |
| `src/state.rs` | `App` struct (global state), participant list animations, draw logic |
| `src/render.rs` | EGL context init, shader compilation, text texturing, vertex buffers for UI rects |
| `src/discord.rs` | IPC socket connection, OAuth2 browser flow, token refresh, event subscriptions |
| `src/handlers.rs` | Pointer/keyboard/surface handlers; hit detection for buttons; drag handle logic |
| `src/config.rs` | TOML parsing, hot-reload detection via inotify, default template |

### Data flow

1. **Initialization**: `main.rs` opens Discord socket (or delays until Discord starts)
2. **Event loop**: calloop waits on Wayland events (pointer, keyboard, window resizes) and IPC socket
3. **Discord updates**: IPC socket fires voice state changes → `state.rs` updates `ParticipantState` list
4. **Input handling**: Pointer events → `handlers.rs` → check button/drag rects → `state.rs` sends IPC cmd
5. **Render cycle**: calloop frame callback → `state.rs::draw()` → `render.rs` GPU calls
6. **Config changes**: inotify detects `config.toml` write → reload + re-render

### Key types

- **`ParticipantState`**: One voice participant (user_id, username, mute/deafen state, speaking timer, animation progress)
- **`ParticipantStateBuilder`**: Builder for constructing participants from Discord data with sensible defaults
- **`App`** (`src/state.rs`): Global overlay state — participants list, position, opacity, compact mode, dragging state
- **`EglContext`** (`src/render.rs`): OpenGL state machine wrapper — manages display, context, EGLSurface
- **`EglBackend`**: Trait for rendering (abstracts test/real paths)
- **`DiscordEvent`**: Enum of voice/OAuth/socket events dispatched from `discord.rs`

## Key conventions

### Input events and hit testing
- **Pointer move/click**: Routes through `handlers.rs::pointer_handler()`
- **Hit detection**: Query rects returned by `button_rects()`, `button2_rects()`, `drag_handle_rects()`
- **Click-through**: Wayland input region set to exclude non-interactive areas (avatars, text)
- **Drag state**: Stored in `App.dragging` (x/y offset from Super key + pointer delta)

### Discord IPC
- **OAuth2 flow**: On first run, opens browser → user authorizes → localhost callback captures code → exchange for token
- **Token caching**: `~/.cache/hypr-overlay/discord-token.json` — survives app restarts
- **Reconnect logic**: Exponential backoff (1s → 30s) if socket closes
- **Event subscriptions**: Sent on READY opcode — subscribed to `VOICE_CHANNEL_SELECT`, `VOICE_STATE_UPDATE`, `SPEAKING_START/END`, `VOICE_STATE_CREATE/DELETE`

### Rendering
- **GPU resources**: Shader programs, vertex buffers, textures stored in `EglContext`
- **SDF shaders**: Rounded rect outlines computed per-pixel (no GLES2 `fwidth()` — uses fixed epsilon)
- **Text rendering**: Fontdue rasterizes SDF glyphs → uploaded as GL texture → drawn as quads
- **Avatar images**: Fetched from Discord CDN via `ureq` HTTP → decoded as PNG → GL texture

### Config hot-reload
- **File watching**: inotify on `~/.config/hypr-overlay/config.toml`
- **On change**: `config.rs` re-parses TOML → merges with old config (override only changed keys)
- **Immediate effect**: Next frame applies new opacity, colors, font size, row count without restart
- **Paths**: XDG_CONFIG_HOME, XDG_CACHE_HOME, XDG_RUNTIME_DIR respected; Discord socket paths (native, Flatpak, Snap) tried in order

### Code style
- **Logging**: Use `tracing::{info, debug, warn, error}` — never `eprintln!` or `println!`
- **Formatting**: `cargo fmt` enforced pre-commit
- **Warnings**: `cargo clippy` must pass with `-D warnings`
- **Test isolation**: Tests use `#[serial_test::serial]` because Discord socket is global — RUST_TEST_THREADS=1 required
- **Dependencies**: Keep minimal — crate tree already large; justify new deps

### Tests location
- **Unit tests**: Mostly in `src/state.rs` testing `ParticipantState` animations and participant list logic
- **Mock rendering**: `EglBackend` trait allows test implementations without GPU
- **No integration tests**: Would require running Discord IPC server; avoided by focusing on state logic

## Common edits

### Adding a config key
1. Add field to `Config` struct in `src/config.rs`
2. Add to TOML template string in `config_path()` function
3. Update hot-reload to handle new key (or rely on serde defaults)
4. Use in `src/state.rs` or `src/render.rs` as needed

### Adding a Discord voice event
1. Add `DiscordEvent` variant in `src/discord.rs`
2. Implement `EventHandler` for new event type (parse, return Self::YourEvent)
3. Add handler to `get_event_handlers()` vec
4. Match new variant in `App::handle_discord_event()` in `src/state.rs`

### Modifying UI rendering
1. Geometry/hit rects: `src/handlers.rs` (`button_rects`, `button2_rects`, `drag_handle_rects`)
2. Shaders or texture updates: `src/render.rs` (fragment/vertex shaders, texture binding)
3. Draw calls: `src/state.rs::draw()` (calls into render.rs)

### Debugging
Set `RUST_LOG=debug` or `RUST_LOG=hypr_overlay_wl::discord=trace` to see IPC traffic, event dispatching, reconnects. View logs with:
```bash
RUST_LOG=debug cargo run
# or if systemd service:
journalctl --user -u hypr-overlay -f
```

## Testing discord-overlay locally

1. Create a Discord application at https://discord.com/developers/applications
2. Copy **Application ID** → set `discord_client_id` in `~/.config/hypr-overlay/config.toml` (auto-created on first run)
3. Copy **Client Secret** → set `discord_client_secret`
4. Or use env vars: `DISCORD_CLIENT_ID=... DISCORD_CLIENT_SECRET=... cargo run`
5. First run: browser OAuth2 popup → authorize → token cached
6. Join a Discord voice channel → overlay appears on Hyprland layer-shell

If running on CI/non-interactive: tests run against mock Discord events (no real IPC needed).
