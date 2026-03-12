use serde::Deserialize;
use tracing::{error, info, warn};

#[derive(Deserialize, Debug, Clone)]
#[serde(default)]
pub struct Config {
    /// Discord application client ID (from Discord Developer Portal)
    pub discord_client_id: String,
    /// Discord application client secret
    pub discord_client_secret: String,
    /// Initial overlay opacity (0.1–1.0)
    pub opacity: f32,
    /// Maximum participant rows before scrolling kicks in
    pub max_visible_rows: usize,
    /// Initial X position (pixels from left edge of output)
    pub initial_x: i32,
    /// Initial Y position (pixels from top edge of output)
    pub initial_y: i32,
    /// Background colour as [R, G, B] in 0.0–1.0
    pub bg_color: [f32; 3],
    /// Speaking ring colour as [R, G, B]
    pub speaking_color: [f32; 3],
    /// Muted/deafened indicator colour as [R, G, B]
    pub muted_color: [f32; 3],
    /// Font size for participant names (pixels)
    pub font_size: f32,
    /// Start in compact mode (single row of avatars, no controls)
    pub compact_by_default: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            discord_client_id: String::new(),
            discord_client_secret: String::new(),
            opacity: 0.9,
            max_visible_rows: 5,
            initial_x: 20,
            initial_y: 20,
            bg_color: [0.12, 0.13, 0.16],
            speaking_color: [0.23, 0.77, 0.33],
            muted_color: [0.8, 0.15, 0.15],
            font_size: 14.0,
            compact_by_default: false,
        }
    }
}

impl Config {
    pub fn load() -> Self {
        let path = dirs_config_path();
        let mut cfg: Config = match std::fs::read_to_string(&path) {
            Ok(text) => match toml::from_str(&text) {
                Ok(c) => {
                    info!("loaded from {path:?}");
                    c
                }
                Err(e) => {
                    warn!("parse error in {path:?}: {e}, using defaults");
                    Self::default()
                }
            },
            Err(_) => {
                info!("no config file found at {path:?}, using defaults");
                Self::default()
            }
        };
        // Env vars override config file (useful for secrets in systemd unit overrides)
        if let Ok(v) = std::env::var("DISCORD_CLIENT_ID") {
            cfg.discord_client_id = v;
        }
        if let Ok(v) = std::env::var("DISCORD_CLIENT_SECRET") {
            cfg.discord_client_secret = v;
        }
        cfg
    }

    /// Write a default config file if none exists.
    pub fn write_default_if_missing() {
        let mut path = dirs_config_path();
        if path.exists() {
            return;
        }
        // Ensure parent directory exists; if creation fails, try a HOME-based fallback.
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                error!("could not create config dir {:?}: {e}", parent);
                if let Ok(home) = std::env::var("HOME") {
                    let fallback_parent = std::path::PathBuf::from(home)
                        .join(".config")
                        .join("hypr-overlay");
                    if let Err(e2) = std::fs::create_dir_all(&fallback_parent) {
                        error!("fallback config dir creation failed: {e2}");
                    } else {
                        path = fallback_parent.join("config.toml");
                    }
                }
            }
        }

        let content = r#"# hypr-overlay configuration
# All fields are optional — remove any line to use the default.

# Discord application credentials (required).
# Create an app at https://discord.com/developers/applications
# Copy "Application ID" as client_id and OAuth2 → "Client Secret" as client_secret.
discord_client_id     = ""
discord_client_secret = ""

# Overlay opacity when in a voice channel (0.1–1.0)
opacity = 0.9

# Maximum participant rows visible before the list scrolls
max_visible_rows = 5

# Initial position on screen (pixels from top-left of the output)
initial_x = 20
initial_y = 20

# Colours as [R, G, B] in 0.0–1.0 range
bg_color       = [0.12, 0.13, 0.16]
speaking_color = [0.23, 0.77, 0.33]
muted_color    = [0.80, 0.15, 0.15]

# Font size for participant names (pixels)
font_size = 14.0

# Start in compact mode (single row of avatars, no controls)
compact_by_default = false
"#;

        // Try a straightforward write first; if it fails, attempt a create+write.
        match std::fs::write(&path, content) {
            Ok(_) => {
                info!("wrote default config to {path:?}");
                return;
            }
            Err(e) => {
                error!("could not write default config: {e}, attempting create/open");
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                match std::fs::OpenOptions::new().create(true).write(true).truncate(true).open(&path) {
                    Ok(mut f) => {
                        use std::io::Write;
                        if let Err(e2) = f.write_all(content.as_bytes()) {
                            error!("failed to write default config after create: {e2}");
                        } else {
                            info!("wrote default config to {path:?}");
                        }
                    }
                    Err(e3) => {
                        error!("could not create default config file: {e3}");
                    }
                }
            }
        }
    }
}

pub fn config_path() -> std::path::PathBuf {
    dirs_config_path()
}

fn dirs_config_path() -> std::path::PathBuf {
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
            std::path::PathBuf::from(home).join(".config")
        });
    base.join("hypr-overlay").join("config.toml")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::fs;

    #[test]
    fn default_values() {
        let d = Config::default();
        assert_eq!(d.opacity, 0.9);
        assert_eq!(d.max_visible_rows, 5);
        assert_eq!(d.font_size, 14.0);
        assert!(!d.compact_by_default);
    }

    #[test]
    fn write_default_and_load() {
        let tmp = std::env::temp_dir().join(format!("hypr_cfg_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        env::set_var("XDG_CONFIG_HOME", &tmp);
        let cfg_path = config_path();
        if cfg_path.exists() { let _ = fs::remove_file(&cfg_path); }
        Config::write_default_if_missing();
        assert!(cfg_path.exists());
        let cfg = Config::load();
        assert_eq!(cfg.opacity, 0.9);
        let _ = fs::remove_file(&cfg_path);
        let _ = fs::remove_dir_all(&tmp);
        env::remove_var("XDG_CONFIG_HOME");
    }

    #[test]
    fn env_override() {
        let tmp = std::env::temp_dir().join(format!("hypr_cfg_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        env::set_var("XDG_CONFIG_HOME", &tmp);
        env::set_var("DISCORD_CLIENT_ID", "OVERRIDE_ID");
        let cfg = Config::load();
        assert_eq!(cfg.discord_client_id, "OVERRIDE_ID");
        env::remove_var("DISCORD_CLIENT_ID");
        let _ = fs::remove_dir_all(tmp);
        env::remove_var("XDG_CONFIG_HOME");
    }
}
