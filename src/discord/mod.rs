//! Discord IPC client — connects to a running Discord client over its Unix socket,
//! handles OAuth2, and subscribes to voice settings events (mute/deafen).
//!
//! # Setup
//! 1. Create an application at https://discord.com/developers/applications
//! 2. Copy "Application ID" → DISCORD_CLIENT_ID env var
//! 3. OAuth2 tab → copy "Client Secret" → DISCORD_CLIENT_SECRET env var
//! 4. OAuth2 tab → add redirect URL: http://127.0.0.1
//!
//! On first run Discord shows an authorization pop-up; accept it.
//! The token is cached at ~/.cache/hypr-overlay/discord-token.json.

mod auth;
mod client;
mod handlers;
mod ipc;
mod parser;
mod types;

// Public re-exports
pub use client::run_client;
pub use types::{Config, DiscordCommand, DiscordEvent, Participant};

use std::sync::mpsc;

/// Spawn the Discord IPC client in a background thread.
pub fn spawn(
    config: Config,
    tx: calloop::channel::Sender<DiscordEvent>,
    cmd_rx: mpsc::Receiver<DiscordCommand>,
) {
    std::thread::spawn(move || run_client(config, tx, cmd_rx));
}
