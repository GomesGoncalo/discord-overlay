//! Public types for Discord IPC communication.

use serde_json::Value;

pub struct Config {
    pub client_id: String,
    pub client_secret: String,
}

/// A participant in the current voice channel.
#[derive(Debug, Clone)]
pub struct Participant {
    pub user_id: String,
    pub username: String,
    pub nick: Option<String>,
    pub avatar_hash: Option<String>,
    /// True if muted by themselves or by the server.
    pub muted: bool,
    /// True if deafened by themselves or by the server.
    pub deafened: bool,
}

/// Events sent from the Discord thread to the main thread.
#[derive(Debug)]
pub enum DiscordEvent {
    /// Successfully authenticated; contains the Discord username and user ID.
    Ready { username: String, user_id: String },
    /// Current mute/deafen state (sent on connect and on change).
    VoiceSettings { mute: bool, deaf: bool },
    /// Voice input mode: true = push-to-talk, false = voice activity.
    VoiceMode { ptt: bool },
    /// Current participants in the voice channel (full replace).
    VoiceParticipants {
        participants: Vec<Participant>,
        channel_name: Option<String>,
    },
    /// A user joined the voice channel.
    UserJoined(Participant),
    /// A user left the voice channel.
    UserLeft { user_id: String },
    /// A user's mute/deaf state changed.
    ParticipantStateUpdate {
        user_id: String,
        muted: bool,
        deafened: bool,
    },
    /// A user started or stopped speaking.
    SpeakingUpdate { user_id: String, speaking: bool },
    /// Avatar image downloaded and decoded.
    AvatarLoaded {
        user_id: String,
        rgba: Vec<u8>,
        size: u32,
    },
    /// Guild (server) name for the current voice channel.
    GuildName { name: String },
    /// Connection lost.
    Disconnected,
}

/// Commands sent from the main thread to the Discord thread.
#[derive(Debug)]
pub enum DiscordCommand {
    SetMute(bool),
    SetDeaf(bool),
}

/// Helper trait for cleaner JSON value extraction.
/// Reduces repeated `.as_str().unwrap_or("")` and `.as_bool().unwrap_or(false)` patterns.
pub trait JsonExt {
    /// Extract string value, return empty string if missing or not a string.
    fn get_string(&self, key: &str) -> String;

    /// Extract string value as Option, returns None if missing or empty.
    fn get_str_option(&self, key: &str) -> Option<String>;

    /// Extract boolean value with default.
    fn get_bool(&self, key: &str, default: bool) -> bool;

    /// Extract value at nested path like ["data"]["name"]
    #[allow(dead_code)]
    fn get_nested(&self, path: &[&str]) -> Option<Value>;
}

impl JsonExt for Value {
    fn get_string(&self, key: &str) -> String {
        self[key].as_str().unwrap_or("").to_string()
    }

    fn get_str_option(&self, key: &str) -> Option<String> {
        self[key]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
    }

    fn get_bool(&self, key: &str, default: bool) -> bool {
        self[key].as_bool().unwrap_or(default)
    }

    fn get_nested(&self, path: &[&str]) -> Option<Value> {
        let mut current = self.clone();
        for key in path {
            current = current[key].clone();
            if current.is_null() {
                return None;
            }
        }
        Some(current)
    }
}

/// Builder for Participant with sensible defaults.
pub struct ParticipantBuilder {
    user_id: String,
    username: String,
    nick: Option<String>,
    avatar_hash: Option<String>,
    muted: bool,
    deafened: bool,
}

impl ParticipantBuilder {
    pub fn new(user_id: impl Into<String>, username: impl Into<String>) -> Self {
        Self {
            user_id: user_id.into(),
            username: username.into(),
            nick: None,
            avatar_hash: None,
            muted: false,
            deafened: false,
        }
    }

    pub fn nick(mut self, nick: Option<String>) -> Self {
        self.nick = nick;
        self
    }

    pub fn avatar_hash(mut self, hash: Option<String>) -> Self {
        self.avatar_hash = hash;
        self
    }

    pub fn muted(mut self, muted: bool) -> Self {
        self.muted = muted;
        self
    }

    pub fn deafened(mut self, deafened: bool) -> Self {
        self.deafened = deafened;
        self
    }

    pub fn build(self) -> Participant {
        Participant {
            user_id: self.user_id,
            username: self.username,
            nick: self.nick,
            avatar_hash: self.avatar_hash.filter(|h| !h.is_empty()),
            muted: self.muted,
            deafened: self.deafened,
        }
    }
}
