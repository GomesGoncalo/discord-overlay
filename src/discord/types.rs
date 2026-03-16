//! Public types for Discord IPC communication.

use serde_json::Value;
use std::fmt;

pub struct Config {
    pub client_id: String,
    pub client_secret: String,
}

/// Unique Discord user identifier.
/// Wrapping String in a newtype prevents accidental parameter swaps
/// and makes intent clearer at call sites.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct UserId(pub String);

impl UserId {
    #[allow(dead_code)]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn bytes(&self) -> std::str::Bytes<'_> {
        self.0.bytes()
    }
}

impl fmt::Display for UserId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<String> for UserId {
    fn from(s: String) -> Self {
        UserId(s)
    }
}

impl From<&str> for UserId {
    fn from(s: &str) -> Self {
        UserId(s.to_string())
    }
}

impl PartialEq<&str> for UserId {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

impl PartialEq<&str> for &UserId {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

impl PartialEq<String> for UserId {
    fn eq(&self, other: &String) -> bool {
        &self.0 == other
    }
}

/// A participant in the current voice channel.
#[derive(Debug, Clone)]
pub struct Participant {
    pub user_id: UserId,
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
    Ready { username: String, user_id: UserId },
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
    UserLeft { user_id: UserId },
    /// A user's mute/deaf state changed.
    ParticipantStateUpdate {
        user_id: UserId,
        muted: bool,
        deafened: bool,
    },
    /// A user started or stopped speaking.
    SpeakingUpdate { user_id: UserId, speaking: bool },
    /// Avatar image downloaded and decoded.
    AvatarLoaded {
        user_id: UserId,
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
    user_id: UserId,
    username: String,
    nick: Option<String>,
    avatar_hash: Option<String>,
    muted: bool,
    deafened: bool,
}

impl ParticipantBuilder {
    pub fn new(user_id: impl Into<UserId>, username: impl Into<String>) -> Self {
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn userid_as_str() {
        let id = UserId("hello".to_string());
        assert_eq!(id.as_str(), "hello");
    }

    #[test]
    fn userid_from_string() {
        let id: UserId = String::from("abc").into();
        assert_eq!(id, "abc");
    }

    #[test]
    fn userid_from_str_ref() {
        let id: UserId = "xyz".into();
        assert_eq!(id, "xyz");
    }

    #[test]
    fn userid_partial_eq_string() {
        let id = UserId("foo".to_string());
        assert_eq!(id, "foo".to_string());
    }

    #[test]
    fn userid_ref_partial_eq_str() {
        let id = UserId("bar".to_string());
        assert_eq!(&id, "bar");
    }

    #[test]
    fn get_nested_success() {
        let v = json!({"a": {"b": "found"}});
        let result = v.get_nested(&["a", "b"]);
        assert_eq!(result.unwrap().as_str(), Some("found"));
    }

    #[test]
    fn get_nested_missing_key() {
        let v = json!({"a": {"b": "found"}});
        assert!(v.get_nested(&["a", "c"]).is_none());
    }

    #[test]
    fn get_nested_empty_path() {
        let v = json!({"x": 1});
        // Empty path — returns the root value unchanged
        assert!(v.get_nested(&[]).is_some());
    }

    #[test]
    fn get_bool_with_default() {
        let v = json!({"flag": "not-a-bool"});
        assert!(v.get_bool("flag", true)); // non-bool → default
        assert!(!v.get_bool("missing", false)); // missing → default
    }

    #[test]
    fn participant_builder_full() {
        let p = ParticipantBuilder::new("u1", "alice")
            .nick(Some("Alice".to_string()))
            .avatar_hash(Some("hash123".to_string()))
            .muted(true)
            .deafened(true)
            .build();
        assert_eq!(p.user_id, "u1");
        assert_eq!(p.username, "alice");
        assert_eq!(p.nick.as_deref(), Some("Alice"));
        assert_eq!(p.avatar_hash.as_deref(), Some("hash123"));
        assert!(p.muted);
        assert!(p.deafened);
    }

    #[test]
    fn participant_builder_empty_avatar_hash_filtered() {
        let p = ParticipantBuilder::new("u2", "bob")
            .avatar_hash(Some(String::new()))
            .build();
        assert!(p.avatar_hash.is_none());
    }
}
