//! Event handlers for Discord IPC frame processing.

use super::parser::{parse_participants, parse_voice_state};
use super::types::{DiscordEvent, JsonExt, Participant, ParticipantBuilder};
use serde_json::Value;

pub type FrameProcessResult = (
    Vec<DiscordEvent>,
    Vec<(crate::discord::UserId, String)>,
    Option<String>,
    Option<String>,
);

/// Cached data about the local user from participant list.
struct SelfParticipantData {
    nick: Option<String>,
    avatar_hash: Option<String>,
    muted: bool,
    deafened: bool,
}

/// Extract local user's data from participant list in a single pass.
/// Returns the data and a vec of all other participants.
fn extract_self_data(
    participants: &[Participant],
    local_user_id: &str,
    local_avatar: Option<&String>,
) -> (SelfParticipantData, Vec<Participant>) {
    let self_data = participants
        .iter()
        .find(|p| p.user_id == local_user_id)
        .map(|p| SelfParticipantData {
            nick: p.nick.clone(),
            avatar_hash: local_avatar.cloned().or_else(|| p.avatar_hash.clone()),
            muted: p.muted,
            deafened: p.deafened,
        })
        .unwrap_or(SelfParticipantData {
            nick: None,
            avatar_hash: local_avatar.cloned(),
            muted: false,
            deafened: false,
        });

    let others = participants
        .iter()
        .filter(|p| p.user_id != local_user_id)
        .cloned()
        .collect();

    (self_data, others)
}

pub trait EventHandler {
    fn matches(&self, v: &Value) -> bool;
    fn handle(
        &self,
        v: &Value,
        local_user_id: &str,
        local_username: &str,
        local_avatar: Option<&String>,
    ) -> Option<FrameProcessResult>;
}

pub struct GetGuildHandler;
impl EventHandler for GetGuildHandler {
    fn matches(&self, v: &Value) -> bool {
        v.get_string("cmd") == "GET_GUILD" && v.get_string("nonce") == "get_guild"
    }

    fn handle(
        &self,
        v: &Value,
        _local_user_id: &str,
        _local_username: &str,
        _local_avatar: Option<&String>,
    ) -> Option<FrameProcessResult> {
        if let Some(name) = v["data"].get_str_option("name") {
            let events = vec![DiscordEvent::GuildName { name }];
            return Some((events, Vec::new(), None, None));
        }
        None
    }
}

pub struct VoiceChannelSelectHandler;
impl EventHandler for VoiceChannelSelectHandler {
    fn matches(&self, v: &Value) -> bool {
        v.get_string("cmd") == "GET_SELECTED_VOICE_CHANNEL" && v.get_string("nonce") == "gvsc"
    }

    fn handle(
        &self,
        v: &Value,
        local_user_id: &str,
        local_username: &str,
        local_avatar: Option<&String>,
    ) -> Option<FrameProcessResult> {
        if !v["data"].is_null() {
            let subscribe_channel = v["data"].get_str_option("id");

            let all_participants = parse_participants(&v["data"]);
            let (self_data, others) =
                extract_self_data(&all_participants, local_user_id, local_avatar);

            let mut parts =
                vec![
                    ParticipantBuilder::new(local_user_id.to_string(), local_username)
                        .nick(self_data.nick)
                        .avatar_hash(self_data.avatar_hash.clone())
                        .muted(self_data.muted)
                        .deafened(self_data.deafened)
                        .build(),
                ];
            parts.extend(others);

            let channel_name = v["data"]["name"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string());

            let mut avatars = Vec::new();
            for p in &parts {
                if let Some(hash) = &p.avatar_hash {
                    avatars.push((p.user_id.clone(), hash.clone()));
                }
            }

            let guild_id = v["data"]["guild_id"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string());

            let events = vec![DiscordEvent::VoiceParticipants {
                participants: parts,
                channel_name,
            }];

            Some((events, avatars, subscribe_channel, guild_id))
        } else {
            let events = vec![DiscordEvent::VoiceParticipants {
                participants: vec![],
                channel_name: None,
            }];
            Some((events, Vec::new(), None, None))
        }
    }
}

pub struct SpeakingStartHandler;
impl EventHandler for SpeakingStartHandler {
    fn matches(&self, v: &Value) -> bool {
        v.get_string("evt") == "SPEAKING_START"
    }

    fn handle(
        &self,
        v: &Value,
        _local_user_id: &str,
        _local_username: &str,
        _local_avatar: Option<&String>,
    ) -> Option<FrameProcessResult> {
        if let Some(uid) = v["data"].get_str_option("user_id") {
            let events = vec![DiscordEvent::SpeakingUpdate {
                user_id: uid.into(),
                speaking: true,
            }];
            return Some((events, Vec::new(), None, None));
        }
        None
    }
}

pub struct SpeakingEndHandler;
impl EventHandler for SpeakingEndHandler {
    fn matches(&self, v: &Value) -> bool {
        v.get_string("evt") == "SPEAKING_END"
    }

    fn handle(
        &self,
        v: &Value,
        _local_user_id: &str,
        _local_username: &str,
        _local_avatar: Option<&String>,
    ) -> Option<FrameProcessResult> {
        if let Some(uid) = v["data"].get_str_option("user_id") {
            let events = vec![DiscordEvent::SpeakingUpdate {
                user_id: uid.into(),
                speaking: false,
            }];
            return Some((events, Vec::new(), None, None));
        }
        None
    }
}

pub struct VoiceStateUpdateHandler;
impl EventHandler for VoiceStateUpdateHandler {
    fn matches(&self, v: &Value) -> bool {
        v.get_string("evt") == "VOICE_STATE_UPDATE"
    }

    fn handle(
        &self,
        v: &Value,
        _local_user_id: &str,
        _local_username: &str,
        _local_avatar: Option<&String>,
    ) -> Option<FrameProcessResult> {
        let p = parse_voice_state(&v["data"]);
        if !p.user_id.is_empty() {
            let events = vec![DiscordEvent::ParticipantStateUpdate {
                user_id: p.user_id,
                muted: p.muted,
                deafened: p.deafened,
            }];
            return Some((events, Vec::new(), None, None));
        }
        None
    }
}

pub struct VoiceStateCreateHandler;
impl EventHandler for VoiceStateCreateHandler {
    fn matches(&self, v: &Value) -> bool {
        v.get_string("evt") == "VOICE_STATE_CREATE"
    }

    fn handle(
        &self,
        v: &Value,
        _local_user_id: &str,
        _local_username: &str,
        _local_avatar: Option<&String>,
    ) -> Option<FrameProcessResult> {
        let p = parse_voice_state(&v["data"]);
        if !p.user_id.is_empty() {
            let mut avatars = Vec::new();
            if let Some(hash) = &p.avatar_hash {
                avatars.push((p.user_id.clone(), hash.clone()));
            }
            let events = vec![DiscordEvent::UserJoined(p)];
            return Some((events, avatars, None, None));
        }
        None
    }
}

pub struct VoiceStateDeleteHandler;
impl EventHandler for VoiceStateDeleteHandler {
    fn matches(&self, v: &Value) -> bool {
        v.get_string("evt") == "VOICE_STATE_DELETE"
    }

    fn handle(
        &self,
        v: &Value,
        _local_user_id: &str,
        _local_username: &str,
        _local_avatar: Option<&String>,
    ) -> Option<FrameProcessResult> {
        if let Some(uid) = v["data"]["user"]["id"].as_str() {
            let events = vec![DiscordEvent::UserLeft {
                user_id: uid.into(),
            }];
            return Some((events, Vec::new(), None, None));
        }
        None
    }
}

pub struct VoiceChannelSelectEventHandler;
impl EventHandler for VoiceChannelSelectEventHandler {
    fn matches(&self, v: &Value) -> bool {
        v.get_string("evt") == "VOICE_CHANNEL_SELECT"
    }

    fn handle(
        &self,
        v: &Value,
        _local_user_id: &str,
        _local_username: &str,
        _local_avatar: Option<&String>,
    ) -> Option<FrameProcessResult> {
        if let Some(cid) = v["data"].get_str_option("channel_id") {
            Some((Vec::new(), Vec::new(), Some(cid), None))
        } else {
            let events = vec![
                DiscordEvent::GuildName {
                    name: String::new(),
                },
                DiscordEvent::VoiceParticipants {
                    participants: vec![],
                    channel_name: None,
                },
            ];
            Some((events, Vec::new(), None, None))
        }
    }
}

pub fn get_event_handlers() -> Vec<Box<dyn EventHandler>> {
    vec![
        Box::new(GetGuildHandler),
        Box::new(VoiceChannelSelectHandler),
        Box::new(SpeakingStartHandler),
        Box::new(SpeakingEndHandler),
        Box::new(VoiceStateUpdateHandler),
        Box::new(VoiceStateCreateHandler),
        Box::new(VoiceStateDeleteHandler),
        Box::new(VoiceChannelSelectEventHandler),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn process_frame_speaking_start() {
        let v = json!({"evt": "SPEAKING_START", "data": {"user_id": "u1"}});
        let h = SpeakingStartHandler;
        let res = h.handle(&v, "local", "me", None);
        assert!(res.is_some());
        let (events, _, _, _) = res.unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            DiscordEvent::SpeakingUpdate { user_id, speaking } => {
                assert!(user_id == "u1");
                assert!(*speaking);
            }
            _ => panic!("expected SpeakingUpdate"),
        }
    }

    #[test]
    fn process_frame_speaking_end() {
        let v = json!({"evt": "SPEAKING_END", "data": {"user_id": "u2"}});
        let h = SpeakingEndHandler;
        let res = h.handle(&v, "local", "me", None);
        assert!(res.is_some());
        let (events, _, _, _) = res.unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            DiscordEvent::SpeakingUpdate { user_id, speaking } => {
                assert!(user_id == "u2");
                assert!(!*speaking);
            }
            _ => panic!("expected SpeakingUpdate"),
        }
    }

    #[test]
    fn process_frame_voice_state_update() {
        let vs = json!({
            "user": {"id": "u3", "username": "dave"},
            "voice_state": {"self_mute": true, "self_deaf": false, "mute": false, "deaf": false}
        });
        let v = json!({"evt": "VOICE_STATE_UPDATE", "data": vs});
        let h = VoiceStateUpdateHandler;
        let res = h.handle(&v, "local", "me", None);
        assert!(res.is_some());
        let (events, _, _, _) = res.unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            DiscordEvent::ParticipantStateUpdate {
                user_id,
                muted,
                deafened,
            } => {
                assert!(user_id == "u3");
                assert!(*muted);
                assert!(!*deafened);
            }
            _ => panic!("expected ParticipantStateUpdate"),
        }
    }

    #[test]
    fn process_frame_voice_state_create() {
        let vs = json!({
            "user": {"id": "u4", "username": "eve", "avatar": "ehash"},
            "voice_state": {"self_mute": false, "self_deaf": false, "mute": false, "deaf": false}
        });
        let v = json!({"evt": "VOICE_STATE_CREATE", "data": vs});
        let h = VoiceStateCreateHandler;
        let res = h.handle(&v, "local", "me", None);
        assert!(res.is_some());
        let (events, avatars, _, _) = res.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(avatars.len(), 1);
        assert_eq!(avatars[0].0, "u4");
        assert_eq!(avatars[0].1, "ehash");
        match &events[0] {
            DiscordEvent::UserJoined(p) => {
                assert_eq!(p.user_id, "u4");
                assert_eq!(p.username, "eve");
            }
            _ => panic!("expected UserJoined"),
        }
    }

    #[test]
    fn process_frame_voice_state_delete() {
        let v = json!({"evt": "VOICE_STATE_DELETE", "data": {"user": {"id": "u5"}}});
        let h = VoiceStateDeleteHandler;
        let res = h.handle(&v, "local", "me", None);
        assert!(res.is_some());
        let (events, _, _, _) = res.unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            DiscordEvent::UserLeft { user_id } => {
                assert!(user_id == "u5");
            }
            _ => panic!("expected UserLeft"),
        }
    }

    #[test]
    fn process_frame_voice_channel_select_event() {
        let v = json!({"evt": "VOICE_CHANNEL_SELECT", "data": {"channel_id": "ch1"}});
        let h = VoiceChannelSelectEventHandler;
        let res = h.handle(&v, "local", "me", None);
        assert!(res.is_some());
        let (events, _, subscribe, _) = res.unwrap();
        assert!(events.is_empty());
        assert_eq!(subscribe, Some("ch1".to_string()));
    }

    #[test]
    fn process_frame_voice_channel_select_event_empty() {
        let v = json!({"evt": "VOICE_CHANNEL_SELECT", "data": {"channel_id": ""}});
        let h = VoiceChannelSelectEventHandler;
        let res = h.handle(&v, "local", "me", None);
        assert!(res.is_some());
        let (events, _, _, _) = res.unwrap();
        assert_eq!(events.len(), 2);
        match (&events[0], &events[1]) {
            (
                DiscordEvent::GuildName { name },
                DiscordEvent::VoiceParticipants {
                    participants,
                    channel_name,
                },
            ) => {
                assert!(name.is_empty());
                assert!(participants.is_empty());
                assert!(channel_name.is_none());
            }
            _ => panic!("expected GuildName and VoiceParticipants"),
        }
    }

    #[test]
    fn process_frame_get_guild() {
        let v = json!({"cmd": "GET_GUILD", "nonce": "get_guild", "data": {"name": "MyServer"}});
        let h = GetGuildHandler;
        let res = h.handle(&v, "local", "me", None);
        assert!(res.is_some());
        let (events, _, _, _) = res.unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            DiscordEvent::GuildName { name } => {
                assert_eq!(name, "MyServer");
            }
            _ => panic!("expected GuildName"),
        }
    }

    #[test]
    fn process_frame_voice_channel_select_nonempty() {
        let data = json!({
            "id": "chan2",
            "name": "General",
            "guild_id": "g2",
            "voice_states": [
                { "user": { "id": "u1", "username": "alice" }, "voice_state": {} }
            ]
        });
        let v = json!({"cmd": "GET_SELECTED_VOICE_CHANNEL", "nonce": "gvsc", "data": data});
        let h = VoiceChannelSelectHandler;
        let res = h.handle(&v, "u1", "alice", None);
        assert!(res.is_some());
        let (events, _, subscribe, guild) = res.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(subscribe, Some("chan2".to_string()));
        assert_eq!(guild, Some("g2".to_string()));
    }

    // --- None-return paths ---

    #[test]
    fn get_guild_handler_missing_name_returns_none() {
        let v = json!({"cmd": "GET_GUILD", "nonce": "get_guild", "data": {}});
        let h = GetGuildHandler;
        assert!(h.handle(&v, "", "", None).is_none());
    }

    #[test]
    fn speaking_start_handler_missing_user_id_returns_none() {
        let v = json!({"evt": "SPEAKING_START", "data": {}});
        let h = SpeakingStartHandler;
        assert!(h.handle(&v, "", "", None).is_none());
    }

    #[test]
    fn speaking_end_handler_missing_user_id_returns_none() {
        let v = json!({"evt": "SPEAKING_END", "data": {}});
        let h = SpeakingEndHandler;
        assert!(h.handle(&v, "", "", None).is_none());
    }

    #[test]
    fn voice_state_update_handler_empty_user_id_returns_none() {
        let v =
            json!({"evt": "VOICE_STATE_UPDATE", "data": {"user": {"id": ""}, "voice_state": {}}});
        let h = VoiceStateUpdateHandler;
        assert!(h.handle(&v, "", "", None).is_none());
    }

    #[test]
    fn voice_state_create_handler_empty_user_id_returns_none() {
        let v =
            json!({"evt": "VOICE_STATE_CREATE", "data": {"user": {"id": ""}, "voice_state": {}}});
        let h = VoiceStateCreateHandler;
        assert!(h.handle(&v, "", "", None).is_none());
    }

    #[test]
    fn voice_state_delete_handler_missing_user_returns_none() {
        let v = json!({"evt": "VOICE_STATE_DELETE", "data": {}});
        let h = VoiceStateDeleteHandler;
        assert!(h.handle(&v, "", "", None).is_none());
    }

    #[test]
    fn voice_channel_select_handler_null_data_returns_empty_participants() {
        let v = json!({"cmd": "GET_SELECTED_VOICE_CHANNEL", "nonce": "gvsc", "data": null});
        let h = VoiceChannelSelectHandler;
        let res = h.handle(&v, "local", "me", None);
        assert!(res.is_some());
        let (events, _, subscribe, _) = res.unwrap();
        assert_eq!(events.len(), 1);
        assert!(subscribe.is_none());
        match &events[0] {
            DiscordEvent::VoiceParticipants {
                participants,
                channel_name,
            } => {
                assert!(participants.is_empty());
                assert!(channel_name.is_none());
            }
            _ => panic!("expected VoiceParticipants"),
        }
    }

    // --- extract_self_data fallback (local user not in participant list) ---

    #[test]
    fn extract_self_data_user_not_found_uses_fallback() {
        use crate::discord::types::ParticipantBuilder;
        let participants = vec![ParticipantBuilder::new("other_user", "alice").build()];
        let (self_data, others) = extract_self_data(&participants, "local", None);
        assert!(self_data.nick.is_none());
        assert!(self_data.avatar_hash.is_none());
        assert!(!self_data.muted);
        assert!(!self_data.deafened);
        assert_eq!(others.len(), 1);
        assert_eq!(others[0].user_id, "other_user");
    }

    #[test]
    fn extract_self_data_with_local_avatar_fallback() {
        use crate::discord::types::ParticipantBuilder;
        let participants = vec![ParticipantBuilder::new("other_user", "alice").build()];
        let local_avatar = "avatar_hash".to_string();
        let (self_data, _) = extract_self_data(&participants, "local", Some(&local_avatar));
        assert_eq!(self_data.avatar_hash.as_deref(), Some("avatar_hash"));
    }

    // --- matches() coverage ---

    #[test]
    fn handler_matches_functions() {
        let guild_v = json!({"cmd": "GET_GUILD", "nonce": "get_guild"});
        assert!(GetGuildHandler.matches(&guild_v));
        assert!(!GetGuildHandler.matches(&json!({"cmd": "OTHER"})));

        let gvsc_v = json!({"cmd": "GET_SELECTED_VOICE_CHANNEL", "nonce": "gvsc"});
        assert!(VoiceChannelSelectHandler.matches(&gvsc_v));

        let ss_v = json!({"evt": "SPEAKING_START"});
        assert!(SpeakingStartHandler.matches(&ss_v));

        let se_v = json!({"evt": "SPEAKING_END"});
        assert!(SpeakingEndHandler.matches(&se_v));

        let vsu_v = json!({"evt": "VOICE_STATE_UPDATE"});
        assert!(VoiceStateUpdateHandler.matches(&vsu_v));

        let vsc_v = json!({"evt": "VOICE_STATE_CREATE"});
        assert!(VoiceStateCreateHandler.matches(&vsc_v));

        let vsd_v = json!({"evt": "VOICE_STATE_DELETE"});
        assert!(VoiceStateDeleteHandler.matches(&vsd_v));

        let vcse_v = json!({"evt": "VOICE_CHANNEL_SELECT"});
        assert!(VoiceChannelSelectEventHandler.matches(&vcse_v));
    }

    #[test]
    fn get_event_handlers_returns_eight() {
        let handlers = get_event_handlers();
        assert_eq!(handlers.len(), 8);
    }
}
