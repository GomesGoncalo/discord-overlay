# discord-overlay

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Build](https://img.shields.io/badge/build-passing-brightgreen)](#build--run)

A Wayland-native Discord voice overlay for Hyprland, written in Rust.

Displays your current voice channel participants with speaking indicators, mute/deafen controls, and smooth join/leave animations — rendered directly on a `zwlr_layer_shell_v1` surface with EGL/GLES2.

## Features

- **Voice participant list** — avatars (fetched from Discord CDN), display names, per-user mute/deafen icons
- **Speaking indicators** — green ring around avatar while a participant is speaking (1.5 s timeout)
- **Mute / Deafen buttons** — click to toggle; state synced from Discord in real time
- **Guild + channel name** — shown in the control bar with session duration timer
- **Compact mode** — double-click drag handle for a minimal avatar-only strip
- **Deafened avatars** — rendered in greyscale as an instant visual cue
- **Smooth animations** — participants slide in/out on join/leave (~180 ms)
- **Hidden when idle** — fully transparent and click-through when not in a voice channel
- **Scroll to adjust opacity** — scroll wheel over the control bar changes global opacity (0.1 – 1.0)
- **Scrollable participant list** — scroll over participant area when there are more than `max_visible_rows`
- **Drag to reposition** — hold Super and drag anywhere on screen
- **Click-through** — only buttons and drag handle are interactive; the rest passes clicks through
- **Auto-reconnect** — exponential backoff (1 s → 30 s) if Discord restarts
- **Token refresh** — detects expired OAuth tokens and re-authenticates automatically
- **Config hot-reload** — edit `~/.config/hypr-overlay/config.toml` and changes apply live
- **systemd user service** — autostart with your Hyprland session

## Architecture

| Layer | Technology |
|---|---|
| Wayland surface | `zwlr_layer_shell_v1` via smithay-client-toolkit |
| Rendering | EGL + GLES2 via `glow`; SDF shaders for rounded rects and icons |
| Event loop | `calloop` + `calloop-wayland-source` |
| Discord IPC | Unix socket RPC (OAuth2 `rpc`, `rpc.voice.read`, `rpc.voice.write`) |
| Text rendering | `fontdue` (NotoSans or first available system font) |
| Avatar images | `ureq` HTTP + `image` PNG decode, uploaded as GL textures |
| Logging | `tracing` + `tracing-subscriber` (RUST_LOG filter) |

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

## Install & Autostart

```bash
make install
systemctl --user enable --now hypr-overlay
```

To update a running installation:
```bash
make reinstall
```

To check status or logs:
```bash
systemctl --user status hypr-overlay
journalctl --user -u hypr-overlay -f
```

To uninstall:
```bash
make uninstall
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
| `compact_by_default` | `false` | Start in compact mode |

Get your credentials at <https://discord.com/developers/applications> — create an app, copy the **Application ID** as `discord_client_id`, then go to OAuth2 and copy the **Client Secret** as `discord_client_secret`.

## Environment variables

| Variable | Default | Description |
|---|---|---|
| `OVERLAY_OPACITY` | — | Overrides `opacity` from config |
| `DISCORD_CLIENT_ID` | — | Overrides `discord_client_id` from config |
| `DISCORD_CLIENT_SECRET` | — | Overrides `discord_client_secret` from config |
| `RUST_LOG` | `hypr_overlay_wl=info` | Log level filter (e.g. `debug`, `hypr_overlay_wl::discord=trace`) |

## Controls

| Action | Gesture |
|---|---|
| Toggle mute | Click mic button |
| Toggle deafen | Click headphone button |
| Adjust opacity | Scroll wheel over control bar |
| Scroll participants | Scroll wheel over participant list |
| Move overlay | Hold Super + drag |
| Toggle compact mode | Double-click drag handle |

## Hyprland config

The overlay uses `zwlr_layer_shell_v1` (overlay layer) and manages its own position, so no Hyprland window rules are required.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Bug reports, feature requests and pull requests are welcome.

## Changelog

See [CHANGELOG.md](CHANGELOG.md).

## License

MIT — see [LICENSE](LICENSE).

## Notes

- GLES2 has no `fwidth()` — avatar circle AA uses a fixed constant instead.
- `SPEAKING_END` is not a valid Discord RPC event; speaking state expires via a 1.5 s client-side timer.
- EGL display is obtained via the `eglGetDisplay` classic path (Mesa fallback) rather than `eglGetPlatformDisplay`, which fails with `BadParameter` on some Mesa configurations.

