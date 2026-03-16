//! JSON parsing for Discord voice state events.

use serde_json::{json, Value};
use std::os::unix::net::UnixStream;

use super::ipc::send_cmd;
use super::types::{JsonExt, Participant, ParticipantBuilder};

pub fn parse_participants(channel_data: &Value) -> Vec<Participant> {
    let states = channel_data["voice_states"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    states.iter().map(parse_voice_state).collect()
}

pub fn parse_voice_state(vs: &Value) -> Participant {
    let user = &vs["user"];
    let vs_inner = &vs["voice_state"];
    let self_mute = vs_inner.get_bool("self_mute", false);
    let self_deaf = vs_inner.get_bool("self_deaf", false);
    let server_mute = vs.get_bool("mute", false) || vs_inner.get_bool("mute", false);
    let server_deaf = vs_inner.get_bool("deaf", false);

    let username = user
        .get_str_option("username")
        .unwrap_or_else(|| "?".to_string());

    ParticipantBuilder::new(user.get_string("id"), username)
        .nick(vs.get_str_option("nick"))
        .avatar_hash(user.get_str_option("avatar"))
        .muted(self_mute || server_mute)
        .deafened(self_deaf || server_deaf)
        .build()
}

pub fn subscribe_for_channel(stream: &mut UnixStream, channel_id: &str, nonce: &mut u64) {
    *nonce += 1;
    send_cmd(
        stream,
        json!({
            "cmd": "SUBSCRIBE",
            "evt": "SPEAKING_START",
            "args": { "channel_id": channel_id },
            "nonce": format!("ss_{nonce}")
        }),
    );
    *nonce += 1;
    send_cmd(
        stream,
        json!({
            "cmd": "SUBSCRIBE",
            "evt": "SPEAKING_END",
            "args": { "channel_id": channel_id },
            "nonce": format!("se_{nonce}")
        }),
    );
    *nonce += 1;
    send_cmd(
        stream,
        json!({
            "cmd": "SUBSCRIBE",
            "evt": "VOICE_STATE_CREATE",
            "args": { "channel_id": channel_id },
            "nonce": format!("vsc_{nonce}")
        }),
    );
    *nonce += 1;
    send_cmd(
        stream,
        json!({
            "cmd": "SUBSCRIBE",
            "evt": "VOICE_STATE_UPDATE",
            "args": { "channel_id": channel_id },
            "nonce": format!("vsu_{nonce}")
        }),
    );
    *nonce += 1;
    send_cmd(
        stream,
        json!({
            "cmd": "SUBSCRIBE",
            "evt": "VOICE_STATE_DELETE",
            "args": { "channel_id": channel_id },
            "nonce": format!("vsd_{nonce}")
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_voice_state_basic() {
        let vs = json!({
            "user": {"id": "u1", "username": "bob", "avatar": "ahash"},
            "voice_state": {"self_mute": true, "self_deaf": false, "mute": false, "deaf": false},
            "mute": false,
            "nick": "Bobby"
        });
        let p = parse_voice_state(&vs);
        assert_eq!(p.user_id, "u1");
        assert_eq!(p.username, "bob");
        assert_eq!(p.nick.as_deref(), Some("Bobby"));
        assert_eq!(p.avatar_hash.as_deref(), Some("ahash"));
        assert!(p.muted);
        assert!(!p.deafened);
    }

    #[test]
    fn parse_voice_state_missing_user() {
        let vs = json!({ "voice_state": { "self_mute": false } });
        let p = parse_voice_state(&vs);
        assert_eq!(p.user_id, "");
        assert_eq!(p.username, "?");
        assert!(p.nick.is_none());
        assert!(p.avatar_hash.is_none());
        assert!(!p.muted);
        assert!(!p.deafened);
    }

    #[test]
    fn parse_voice_state_server_mute() {
        let vs = json!({
            "user": {"id": "u2", "username": "alice"},
            "voice_state": {"self_mute": false, "self_deaf": false, "mute": true, "deaf": false},
            "mute": false
        });
        let p = parse_voice_state(&vs);
        assert_eq!(p.user_id, "u2");
        assert!(p.muted);
        assert!(!p.deafened);
    }

    #[test]
    fn parse_voice_state_no_voice_state() {
        let vs = json!({ "user": {"id": "u3", "username": "carol", "avatar": ""}, "nick": "" });
        let p = parse_voice_state(&vs);
        assert_eq!(p.user_id, "u3");
        assert_eq!(p.username, "carol");
        assert!(p.avatar_hash.is_none());
        assert!(!p.muted);
        assert!(!p.deafened);
    }

    #[test]
    fn parse_participants_array() {
        let vs1 = json!({ "user": {"id": "u1", "username": "bob"}, "voice_state": {} });
        let vs2 = json!({ "user": {"id": "u2", "username": "alice"}, "voice_state": {} });
        let ch = json!({ "voice_states": [vs1, vs2] });
        let parts = parse_participants(&ch);
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].user_id, "u1");
        assert_eq!(parts[1].user_id, "u2");
    }
}

#[test]
fn parse_voice_state_with_server_deafen() {
    let vs = json!({
        "user": {"id": "u4", "username": "david"},
        "voice_state": {"self_mute": false, "self_deaf": false, "mute": false, "deaf": true},
        "mute": false
    });
    let p = parse_voice_state(&vs);
    assert_eq!(p.user_id, "u4");
    assert!(!p.muted);
    assert!(p.deafened);
}

#[test]
fn parse_voice_state_empty_avatar() {
    let vs = json!({
        "user": {"id": "u5", "username": "eve", "avatar": ""},
        "voice_state": {}
    });
    let p = parse_voice_state(&vs);
    assert_eq!(p.user_id, "u5");
    assert!(p.avatar_hash.is_none());
}

#[test]
fn parse_participants_empty_array() {
    let ch = json!({ "voice_states": [] });
    let parts = parse_participants(&ch);
    assert!(parts.is_empty());
}

#[test]
fn parse_voice_state_self_deaf() {
    use serde_json::json;
    let vs = json!({
        "user": {"id": "u7", "username": "grace"},
        "voice_state": {"self_mute": false, "self_deaf": true, "mute": false, "deaf": false},
        "mute": false
    });
    let p = parse_voice_state(&vs);
    assert_eq!(p.user_id, "u7");
    assert!(!p.muted);
    assert!(p.deafened);
}

#[test]
fn subscribe_for_channel_sends_five_frames() {
    use crate::discord::ipc::read_frame;
    use std::os::unix::net::UnixStream;
    let (mut client, mut server) = UnixStream::pair().unwrap();
    server.set_nonblocking(true).unwrap();
    let mut nonce = 10u64;
    subscribe_for_channel(&mut client, "ch_test", &mut nonce);
    assert_eq!(nonce, 15);
    // Read all frames back to verify they were written correctly
    let mut frames = Vec::new();
    while let Ok((_, v)) = read_frame(&mut server) {
        frames.push(v);
    }
    assert_eq!(frames.len(), 5);
    let events: Vec<&str> = frames
        .iter()
        .map(|f| f["evt"].as_str().unwrap_or(""))
        .collect();
    assert!(events.contains(&"SPEAKING_START"));
    assert!(events.contains(&"SPEAKING_END"));
    assert!(events.contains(&"VOICE_STATE_CREATE"));
    assert!(events.contains(&"VOICE_STATE_UPDATE"));
    assert!(events.contains(&"VOICE_STATE_DELETE"));
}

#[test]
fn parse_participants_multiple_with_nicks() {
    let vs1 =
        json!({ "user": {"id": "u1", "username": "bob"}, "voice_state": {}, "nick": "Bobby" });
    let vs2 =
        json!({ "user": {"id": "u2", "username": "alice"}, "voice_state": {}, "nick": "Ali" });
    let ch = json!({ "voice_states": [vs1, vs2] });
    let parts = parse_participants(&ch);
    assert_eq!(parts.len(), 2);
    assert_eq!(parts[0].nick.as_deref(), Some("Bobby"));
    assert_eq!(parts[1].nick.as_deref(), Some("Ali"));
}
