# Contributing

Thanks for your interest in contributing to hypr-overlay!

## Getting started

```bash
git clone https://github.com/youruser/discord-overlay
cd discord-overlay
cargo build
```

You'll need Discord credentials in `~/.config/hypr-overlay/config.toml` to test IPC features (see README).

## Project structure

| File | Responsibility |
|---|---|
| `src/main.rs` | Entry point, calloop event loop setup |
| `src/render.rs` | EGL/GLES2 context, shaders, SDF icon rasterisation, text rendering |
| `src/state.rs` | `App` struct, `draw()`, Discord event handling, overlay resize |
| `src/handlers.rs` | Wayland input handlers (pointer, keyboard, layer shell) |
| `src/discord.rs` | Discord IPC client — OAuth2, voice state, reconnect logic |
| `src/config.rs` | TOML config loading, hot-reload support |

## Making changes

- **Rendering changes** → `src/render.rs` (shaders) + `src/state.rs` (`draw()`)
- **New Discord events** → `src/discord.rs` (`DiscordEvent` enum + event loop) + `src/state.rs` (`handle_discord_event`)
- **New config keys** → `src/config.rs` (`Config` struct + default template string)
- **Input handling** → `src/handlers.rs`

## Code style

- Run `cargo fmt` before committing
- Run `cargo clippy` and address any warnings
- Keep `eprintln!`/`println!` out — use `tracing::{info, debug, warn, error}` macros
- Prefer small focused functions over large ones
- Don't add new dependencies without good reason — the dep tree is already sizeable

## Submitting a PR

1. Fork the repo and create a branch: `git checkout -b my-feature`
2. Make your changes and ensure `cargo build` and `cargo clippy` are clean
3. Open a pull request with a clear description of what and why

## Reporting bugs

Open an issue with:
- Your Hyprland and Discord versions
- Relevant log output (`journalctl --user -u hypr-overlay -f` or `RUST_LOG=debug cargo run`)
- Steps to reproduce

## Ideas / roadmap

See the open issues for planned features. Some ideas that would be welcome:
- Multiple monitor support (pin to a specific output by name)
- Per-participant volume control (via `SET_USER_VOICE_SETTINGS`)
- Right-click context menu
- Wayland screencopy-based thumbnail previews

## Releasing a new version

1. Bump `version` in `Cargo.toml`
2. Update `CHANGELOG.md`
3. Commit, tag, and push:
   ```bash
   git tag v0.x.0
   git push origin main --tags
   ```
4. Update `PKGBUILD`:
   - Bump `pkgver`
   - Update `sha256sums` with the actual tarball hash:
     ```bash
     curl -sL https://github.com/ggomes/discord-overlay/archive/refs/tags/v0.x.0.tar.gz | sha256sum
     ```
   - Update `.SRCINFO`:
     ```bash
     makepkg --printsrcinfo > .SRCINFO
     ```
5. Push to AUR:
   ```bash
   git clone ssh://aur@aur.archlinux.org/hypr-overlay-wl.git aur-pkg
   cp PKGBUILD .SRCINFO aur-pkg/
   cd aur-pkg && git add . && git commit -m "Update to v0.x.0" && git push
   ```
