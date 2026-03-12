# hypr-overlay

A Wayland-native Discord voice overlay for Hyprland, written in Rust.

Displays your current voice channel participants with speaking indicators, mute/deafen controls, and smooth join/leave animations — rendered directly on a `zwlr_layer_shell_v1` surface with EGL/GLES2.

## Features

- **Voice participant list** — avatars (fetched from Discord CDN), display names, per-user mute/deafen icons
- **Speaking indicators** — green ring around avatar while a participant is speaking (1.5 s timeout)
- **Mute / Deafen buttons** — click to toggle; state synced from Discord in real time
- **Channel name** — shown in the control bar
- **Smooth animations** — participants slide in/out on join/leave (~180 ms)
- **Idle fade** — overlay dims to 30 % opacity when not in a voice channel
- **Scroll to adjust opacity** — scroll wheel over the overlay changes global opacity (0.1 – 1.0)
- **Drag to reposition** — hold Super and drag the overlay anywhere on screen
- **Click-through** — only the control buttons and drag handle are interactive; the rest passes clicks through
- **Auto-reconnect** — reconnects to Discord IPC with exponential backoff (1 s → 30 s) if Discord restarts
- **Token refresh** — detects expired OAuth tokens, clears the cache, and re-authenticates automatically

## Architecture

| Layer | Technology |
|---|---|
| Wayland surface | `zwlr_layer_shell_v1` via smithay-client-toolkit |
| Rendering | EGL + GLES2 via `glow`; SDF shaders for rounded rects and icons |
| Event loop | `calloop` + `calloop-wayland-source` |
| Discord IPC | Unix socket RPC (OAuth2 `rpc`, `rpc.voice.read`, `rpc.voice.write`) |
| Text rendering | `fontdue` (NotoSans or first available system font) |
| Avatar images | `ureq` HTTP + `image` PNG decode, uploaded as GL textures |

## Dependencies

### Rust crates
All managed by Cargo — no manual steps needed.

### System libraries (Arch Linux)
```
sudo pacman -S mesa libwayland-client
```
The EGL implementation is provided by Mesa (`libEGL.so`). On other distros install the equivalent Mesa/EGL package.

### Discord
Discord must be running and the IPC socket must be available at `$XDG_RUNTIME_DIR/discord-ipc-0` (standard for the native client). Flatpak and Snap Discord socket paths are also tried automatically.

## Build & Run

```bash
cargo build --release
cargo run --release
```

On first run an OAuth2 browser window will open for Discord authorisation. The token is cached at `~/.cache/hypr-overlay/discord-token.json` and reused on subsequent runs.

To force re-authentication:
```bash
rm ~/.cache/hypr-overlay/discord-token.json
```

## Autostart with systemd

Install the binary and service:

```bash
# Build release binary
cargo build --release

# Install binary
install -Dm755 target/release/hypr-overlay-wl ~/.local/bin/hypr-overlay-wl

# Install systemd service
install -Dm644 assets/hypr-overlay.service ~/.config/systemd/user/hypr-overlay.service

# Enable and start
systemctl --user daemon-reload
systemctl --user enable --now hypr-overlay
```

To check status or logs:
```bash
systemctl --user status hypr-overlay
journalctl --user -u hypr-overlay -f
```

## Configuration

On first run a default config is written to `~/.config/hypr-overlay/config.toml`.
Edit it to customise the overlay — changes are applied live without restarting.

| Key | Default | Description |
|---|---|---|
| `discord_client_id` | *(required)* | Discord app "Application ID" |
| `discord_client_secret` | *(required)* | Discord app OAuth2 "Client Secret" |
| `opacity` | `0.9` | Opacity when in a voice channel |
| `max_visible_rows` | `5` | Rows before list scrolls |
| `initial_x` / `initial_y` | `20` / `20` | Starting position (px) |
| `bg_color` | `[0.12, 0.13, 0.16]` | Background colour (RGB 0–1) |
| `speaking_color` | `[0.23, 0.77, 0.33]` | Speaking ring colour |
| `muted_color` | `[0.80, 0.15, 0.15]` | Muted/deafened indicator colour |
| `font_size` | `14.0` | Participant name font size (px) |

Get your credentials at <https://discord.com/developers/applications> — create an app, copy the **Application ID** as `discord_client_id`, then go to OAuth2 and copy the **Client Secret** as `discord_client_secret`.

Env vars `DISCORD_CLIENT_ID` and `DISCORD_CLIENT_SECRET` override the config file values (useful for systemd unit drop-ins or CI).

## Environment variables

| Variable | Description |
|---|---|
| `OVERLAY_OPACITY` | Overrides `opacity` from config |
| `DISCORD_CLIENT_ID` | Overrides `discord_client_id` from config |
| `DISCORD_CLIENT_SECRET` | Overrides `discord_client_secret` from config |

## Controls

| Action | Gesture |
|---|---|
| Toggle mute | Click mic button |
| Toggle deafen | Click headphone button |
| Adjust opacity | Scroll wheel anywhere on overlay |
| Move overlay | Hold Super + drag |

## Hyprland config

The overlay uses `zwlr_layer_shell_v1` (overlay layer) and manages its own position, so no Hyprland window rules are required.

## Notes

- GLES2 has no `fwidth()` — avatar circle AA uses a fixed constant instead.
- `SPEAKING_END` is not a valid Discord RPC event; speaking state expires via a 1.5 s client-side timer.
- EGL display is obtained via the `eglGetDisplay` classic path (Mesa fallback) rather than `eglGetPlatformDisplay`, which fails with `BadParameter` on some Mesa configurations.
