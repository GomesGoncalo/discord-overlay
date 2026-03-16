//! Avatar image fetching and caching.
//!
//! Handles downloading Discord avatars from CDN and decoding PNG images
//! to RGBA8 format suitable for GPU texture upload.

use crate::discord::{DiscordEvent, UserId};
use std::io::Read;
use tracing::{debug, warn};

/// Fetch avatar from Discord CDN and send decoded RGBA data via channel.
///
/// Spawns a background thread to avoid blocking the main IPC loop.
/// Handles HTTP fetch, PNG decode, and format conversion.
/// Failures are logged but not propagated (avatars are optional).
pub fn fetch_and_send(user_id: UserId, hash: String, tx: calloop::channel::Sender<DiscordEvent>) {
    let span = tracing::debug_span!("avatar_fetch", %user_id);
    std::thread::spawn(move || {
        let _enter = span.entered();
        let base = base_url();
        let url = format!("{}/{}/{}.png?size=64", base, user_id, hash);
        debug!("fetching avatar");
        match ureq::get(&url).call() {
            Ok(resp) => {
                let mut buf = Vec::new();
                match resp.into_reader().read_to_end(&mut buf) {
                    Ok(_) => match image::load_from_memory(&buf) {
                        Ok(img) => {
                            let rgba = image::DynamicImage::from(img.to_rgba8()).flipv().to_rgba8();
                            let (w, _h) = rgba.dimensions();
                            let size = w;
                            if size > 0 {
                                debug!(size, "avatar decoded and uploaded");
                                let _ = tx.send(DiscordEvent::AvatarLoaded {
                                    user_id,
                                    rgba: rgba.into_raw(),
                                    size,
                                });
                            } else {
                                warn!("avatar has zero dimensions");
                            }
                        }
                        Err(e) => {
                            warn!("failed to decode avatar image: {e}");
                        }
                    },
                    Err(e) => {
                        warn!("failed to read avatar HTTP response: {e}");
                    }
                }
            }
            Err(e) => {
                warn!(%url, "failed to fetch avatar: {e}");
            }
        }
    });
}

/// Get the base URL for Discord avatar CDN.
///
/// Respects `HYPR_AVATAR_BASE_URL` env var for testing/offline scenarios.
fn base_url() -> String {
    std::env::var("HYPR_AVATAR_BASE_URL")
        .unwrap_or_else(|_| "https://cdn.discordapp.com/avatars".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[serial_test::serial]
    fn avatar_base_url_default() {
        std::env::remove_var("HYPR_AVATAR_BASE_URL");
        assert_eq!(base_url(), "https://cdn.discordapp.com/avatars");
    }

    #[test]
    #[serial_test::serial]
    fn avatar_base_url_env_override() {
        std::env::set_var("HYPR_AVATAR_BASE_URL", "http://localhost:8000");
        assert_eq!(base_url(), "http://localhost:8000");
        std::env::remove_var("HYPR_AVATAR_BASE_URL");
    }
}
