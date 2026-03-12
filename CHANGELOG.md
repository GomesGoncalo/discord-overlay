# Changelog

All notable changes to hypr-overlay are documented here.

## [Unreleased]

### Added
- Voice participant list with avatars, display names, speaking rings
- Mute and deafen toggle buttons synced with Discord state
- Per-participant mute/deafen icons
- Deafened participant avatars rendered in greyscale
- Smooth slide-in/slide-out animations for join/leave (~180 ms)
- Compact mode — double-click drag handle for a minimal avatar-only strip
- Channel name and guild name displayed in control bar
- Session duration timer in control bar
- Scrollable participant list (max 5 rows, configurable)
- Scroll wheel opacity control (over control bar)
- Scroll wheel participant list navigation (over participant area)
- Drag to reposition (hold Super + drag handle)
- Config file at `~/.config/hypr-overlay/config.toml` with hot-reload
- Auto-reconnect with exponential backoff when Discord restarts
- Automatic OAuth2 token refresh on expiry
- Overlay hides completely (fully transparent + click-through) when not in a voice channel
- Smooth fade animation when entering/leaving voice channels
- systemd user service for autostart
- Structured logging via `tracing` crate (RUST_LOG filter support)
- `make install` / `make reinstall` / `make uninstall` targets
