//! Discord IPC client main loop.

use serde_json::{json, Value};
use std::os::unix::net::UnixStream;
use std::sync::mpsc;
use std::time::Duration;
use tracing::{debug, info, trace, warn};

use super::auth::try_auth;
use super::handlers::{get_event_handlers, FrameProcessResult};
use super::ipc::{
    find_socket, is_timeout, read_frame, send_cmd, write_frame, OP_FRAME, OP_HANDSHAKE,
};
use super::parser::{parse_participants, parse_voice_state, subscribe_for_channel};
use super::types::{Config, DiscordCommand, DiscordEvent};
use crate::avatar;

pub fn run_client(
    cfg: Config,
    tx: calloop::channel::Sender<DiscordEvent>,
    cmd_rx: mpsc::Receiver<DiscordCommand>,
) {
    let mut backoff_secs = 1u64;
    loop {
        info!("connecting...");
        match try_connect(&cfg, &tx, &cmd_rx) {
            Ok(()) => {
                info!("disconnected cleanly, reconnecting...");
                backoff_secs = 1;
            }
            Err(()) => {
                warn!("connection error, retrying in {backoff_secs}s");
            }
        }
        let _ = tx.send(DiscordEvent::Disconnected);
        std::thread::sleep(Duration::from_secs(backoff_secs));
        backoff_secs = (backoff_secs * 2).min(30);
    }
}

/// Subscribe to voice settings and channel changes.
pub fn subscribe_initial(stream: &mut UnixStream) {
    send_cmd(
        stream,
        json!({
            "cmd": "GET_VOICE_SETTINGS",
            "args": {},
            "nonce": "gvs"
        }),
    );
    send_cmd(
        stream,
        json!({
            "cmd":  "SUBSCRIBE",
            "evt":  "VOICE_SETTINGS_UPDATE",
            "args": {},
            "nonce": "sub_vsu"
        }),
    );
    send_cmd(
        stream,
        json!({
            "cmd": "SUBSCRIBE",
            "evt": "VOICE_CHANNEL_SELECT",
            "args": {},
            "nonce": "sub_vcs"
        }),
    );
    send_cmd(
        stream,
        json!({
            "cmd": "GET_SELECTED_VOICE_CHANNEL",
            "args": {},
            "nonce": "gvsc"
        }),
    );
}

fn try_connect(
    cfg: &Config,
    tx: &calloop::channel::Sender<DiscordEvent>,
    cmd_rx: &mpsc::Receiver<DiscordCommand>,
) -> Result<(), ()> {
    let _span = tracing::info_span!("discord_connect").entered();
    let mut stream = loop {
        match find_socket() {
            Some(s) => break s,
            None => {
                info!("Discord not found, retrying in 5s...");
                std::thread::sleep(Duration::from_secs(5));
            }
        }
    };

    stream
        .set_read_timeout(Some(Duration::from_millis(50)))
        .ok();

    let handshake = json!({"v": 1, "client_id": cfg.client_id}).to_string();
    write_frame(&mut stream, OP_HANDSHAKE, &handshake).map_err(|_| ())?;

    let ready = wait_for_ready(&mut stream)?;
    let local_user_id = ready["data"]["user"]["id"]
        .as_str()
        .unwrap_or("")
        .to_string();
    let local_username = ready["data"]["user"]["username"]
        .as_str()
        .unwrap_or("unknown")
        .to_string();
    let local_avatar = ready["data"]["user"]["avatar"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    info!("handshake OK — user: {local_username} ({local_user_id})");

    try_auth(cfg, &mut stream).ok_or(())?;
    info!("authenticated");
    let _ = tx.send(DiscordEvent::Ready {
        username: local_username.clone(),
        user_id: local_user_id.clone().into(),
    });

    subscribe_initial(&mut stream);

    let mut nonce: u64 = 1000;
    loop {
        while let Ok(cmd) = cmd_rx.try_recv() {
            nonce += 1;
            match cmd {
                DiscordCommand::SetMute(m) => send_cmd(
                    &mut stream,
                    json!({
                        "cmd":  "SET_VOICE_SETTINGS",
                        "args": { "mute": m },
                        "nonce": nonce.to_string()
                    }),
                ),
                DiscordCommand::SetDeaf(d) => send_cmd(
                    &mut stream,
                    json!({
                        "cmd":  "SET_VOICE_SETTINGS",
                        "args": { "deaf": d },
                        "nonce": nonce.to_string()
                    }),
                ),
            }
        }

        match read_frame(&mut stream) {
            Ok((OP_FRAME, v)) => {
                let cmd = v["cmd"].as_str().unwrap_or("");
                let evt = v["evt"].as_str().unwrap_or("");
                let vnonce = v["nonce"].as_str().unwrap_or("");
                trace!(cmd, evt, nonce = vnonce, "ipc frame received");

                let (events, avatars, subscribe_channel, guild_id) = process_frame_events(
                    &v,
                    &local_user_id,
                    &local_username,
                    local_avatar.as_ref(),
                );
                if !events.is_empty()
                    || !avatars.is_empty()
                    || subscribe_channel.is_some()
                    || guild_id.is_some()
                {
                    for (uid, hash) in avatars {
                        avatar::fetch_and_send(uid, hash, tx.clone());
                    }
                    for e in events {
                        let _ = tx.send(e);
                    }
                    if let Some(cid) = subscribe_channel {
                        subscribe_for_channel(&mut stream, &cid, &mut nonce);
                    }
                    if let Some(gid) = guild_id {
                        send_cmd(
                            &mut stream,
                            json!({
                                "cmd": "GET_GUILD",
                                "args": { "guild_id": gid },
                                "nonce": "get_guild"
                            }),
                        );
                    }
                    continue;
                }

                if cmd == "GET_GUILD" && vnonce == "get_guild" {
                    if let Some(name) = v["data"]["name"].as_str() {
                        let _ = tx.send(DiscordEvent::GuildName {
                            name: name.to_string(),
                        });
                    }
                } else if cmd == "GET_SELECTED_VOICE_CHANNEL" && vnonce == "gvsc" {
                    debug!("gvsc data: {}", v["data"]);
                    if !v["data"].is_null() {
                        let cid = v["data"]["id"].as_str().unwrap_or("").to_string();
                        if !cid.is_empty() {
                            debug!("subscribing SPEAKING_START for channel {cid}");
                            subscribe_for_channel(&mut stream, &cid, &mut nonce);
                        }
                        let others = parse_participants(&v["data"]);
                        let self_nick = others
                            .iter()
                            .find(|p| p.user_id == local_user_id)
                            .and_then(|p| p.nick.clone());
                        let self_avatar = local_avatar.clone().or_else(|| {
                            others
                                .iter()
                                .find(|p| p.user_id == local_user_id)
                                .and_then(|p| p.avatar_hash.clone())
                        });
                        let self_muted = others
                            .iter()
                            .find(|p| p.user_id == local_user_id)
                            .map(|p| p.muted)
                            .unwrap_or(false);
                        let self_deafened = others
                            .iter()
                            .find(|p| p.user_id == local_user_id)
                            .map(|p| p.deafened)
                            .unwrap_or(false);
                        let mut parts = vec![super::types::Participant {
                            user_id: local_user_id.as_str().into(),
                            username: local_username.clone(),
                            nick: self_nick,
                            avatar_hash: self_avatar,
                            muted: self_muted,
                            deafened: self_deafened,
                        }];
                        parts.extend(others.into_iter().filter(|p| p.user_id != local_user_id));
                        let channel_name = v["data"]["name"]
                            .as_str()
                            .filter(|s| !s.is_empty())
                            .map(|s| s.to_string());
                        debug!(
                            "[discord] {} participant(s) in channel {:?}",
                            parts.len(),
                            channel_name
                        );
                        for p in &parts {
                            if let Some(hash) = &p.avatar_hash {
                                avatar::fetch_and_send(p.user_id.clone(), hash.clone(), tx.clone());
                            }
                        }
                        let _ = tx.send(DiscordEvent::VoiceParticipants {
                            participants: parts,
                            channel_name,
                        });
                        if let Some(guild_id) = v["data"]["guild_id"].as_str() {
                            if !guild_id.is_empty() {
                                send_cmd(
                                    &mut stream,
                                    json!({
                                        "cmd": "GET_GUILD",
                                        "args": { "guild_id": guild_id },
                                        "nonce": "get_guild"
                                    }),
                                );
                            }
                        }
                    } else {
                        debug!("not in a voice channel");
                        let _ = tx.send(DiscordEvent::VoiceParticipants {
                            participants: vec![],
                            channel_name: None,
                        });
                    }
                } else if evt == "VOICE_CHANNEL_SELECT" {
                    let cid = v["data"]["channel_id"].as_str().unwrap_or("");
                    debug!("VOICE_CHANNEL_SELECT channel_id={cid:?}");
                    if !cid.is_empty() {
                        send_cmd(
                            &mut stream,
                            json!({
                                "cmd": "GET_SELECTED_VOICE_CHANNEL",
                                "args": {},
                                "nonce": "gvsc"
                            }),
                        );
                    } else {
                        let _ = tx.send(DiscordEvent::GuildName {
                            name: String::new(),
                        });
                        let _ = tx.send(DiscordEvent::VoiceParticipants {
                            participants: vec![],
                            channel_name: None,
                        });
                    }
                } else if evt == "SPEAKING_START" {
                    if let Some(uid) = v["data"]["user_id"].as_str() {
                        trace!(user_id = uid, "speaking start");
                        let _ = tx.send(DiscordEvent::SpeakingUpdate {
                            user_id: uid.into(),
                            speaking: true,
                        });
                    }
                } else if evt == "SPEAKING_END" {
                    if let Some(uid) = v["data"]["user_id"].as_str() {
                        trace!(user_id = uid, "speaking end");
                        let _ = tx.send(DiscordEvent::SpeakingUpdate {
                            user_id: uid.into(),
                            speaking: false,
                        });
                    }
                } else if evt == "VOICE_STATE_UPDATE" {
                    debug!("VOICE_STATE_UPDATE data={}", v["data"]);
                    let p = parse_voice_state(&v["data"]);
                    if !p.user_id.is_empty() {
                        let _ = tx.send(DiscordEvent::ParticipantStateUpdate {
                            user_id: p.user_id,
                            muted: p.muted,
                            deafened: p.deafened,
                        });
                    }
                } else if evt == "VOICE_STATE_CREATE" {
                    debug!("VOICE_STATE_CREATE data={}", v["data"]);
                    let p = parse_voice_state(&v["data"]);
                    if !p.user_id.is_empty() {
                        if let Some(hash) = &p.avatar_hash {
                            avatar::fetch_and_send(p.user_id.clone(), hash.clone(), tx.clone());
                        }
                        let _ = tx.send(DiscordEvent::UserJoined(p));
                    }
                } else if evt == "VOICE_STATE_DELETE" {
                    debug!("VOICE_STATE_DELETE data={}", v["data"]);
                    if let Some(uid) = v["data"]["user"]["id"].as_str() {
                        let _ = tx.send(DiscordEvent::UserLeft {
                            user_id: uid.into(),
                        });
                    }
                } else if evt == "ERROR" {
                    debug!(
                        "[discord] ERROR cmd={cmd:?} nonce={vnonce:?} msg={}",
                        v["data"]["message"]
                    );
                } else {
                    dispatch_event(&v, tx);
                }
            }
            Ok(_) => {}
            Err(e) if is_timeout(&e) => {}
            Err(e) => {
                warn!("read error: {e}");
                return Ok(());
            }
        }
    }
}

fn wait_for_ready(stream: &mut UnixStream) -> Result<Value, ()> {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        if std::time::Instant::now() > deadline {
            return Err(());
        }
        match read_frame(stream) {
            Ok((OP_FRAME, v)) if v["evt"] == "READY" => return Ok(v),
            Ok(_) => continue,
            Err(e) if is_timeout(&e) => continue,
            Err(_) => return Err(()),
        }
    }
}

fn process_frame_events(
    v: &Value,
    local_user_id: &str,
    local_username: &str,
    local_avatar: Option<&String>,
) -> FrameProcessResult {
    let handlers = get_event_handlers();

    for handler in handlers {
        if handler.matches(v) {
            if let Some(result) = handler.handle(v, local_user_id, local_username, local_avatar) {
                return result;
            }
        }
    }

    (Vec::new(), Vec::new(), None, None)
}

fn dispatch_event(v: &Value, tx: &calloop::channel::Sender<DiscordEvent>) {
    match v["evt"].as_str() {
        Some("VOICE_SETTINGS_UPDATE") => {
            let mute = v["data"]["mute"].as_bool().unwrap_or(false);
            let deaf = v["data"]["deaf"].as_bool().unwrap_or(false);
            let _ = tx.send(DiscordEvent::VoiceSettings { mute, deaf });
            if let Some(mode_type) = v["data"]["mode"]["type"].as_str() {
                let ptt = mode_type == "PUSH_TO_TALK";
                let _ = tx.send(DiscordEvent::VoiceMode { ptt });
            }
        }
        _ => {
            if v["cmd"] == "GET_VOICE_SETTINGS" && v["nonce"] == "gvs" {
                let mute = v["data"]["mute"].as_bool().unwrap_or(false);
                let deaf = v["data"]["deaf"].as_bool().unwrap_or(false);
                let _ = tx.send(DiscordEvent::VoiceSettings { mute, deaf });
                if let Some(mode_type) = v["data"]["mode"]["type"].as_str() {
                    let ptt = mode_type == "PUSH_TO_TALK";
                    let _ = tx.send(DiscordEvent::VoiceMode { ptt });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::handlers::EventHandler;
    use super::*;
    use serde_json::json;
    use std::io::{self, Cursor};

    #[test]
    fn is_timeout_kinds() {
        let e1 = io::Error::new(io::ErrorKind::WouldBlock, "");
        let e2 = io::Error::new(io::ErrorKind::TimedOut, "");
        let e3 = io::Error::other("");
        assert!(is_timeout(&e1));
        assert!(is_timeout(&e2));
        assert!(!is_timeout(&e3));
    }

    #[test]
    fn dispatch_event_get_voice_settings() {
        let (tx, rx) = calloop::channel::channel::<DiscordEvent>();
        let v = json!({
            "cmd": "GET_VOICE_SETTINGS",
            "nonce": "gvs",
            "data": { "mute": true, "deaf": false, "mode": { "type": "PUSH_TO_TALK" } }
        });
        dispatch_event(&v, &tx);
        let e1 = rx.recv().unwrap();
        match e1 {
            DiscordEvent::VoiceSettings { mute, deaf } => {
                assert!(mute);
                assert!(!deaf);
            }
            _ => panic!("expected VoiceSettings"),
        }
        let e2 = rx.recv().unwrap();
        match e2 {
            DiscordEvent::VoiceMode { ptt } => assert!(ptt),
            _ => panic!("expected VoiceMode"),
        }
    }

    #[test]
    fn dispatch_event_voice_settings_update_without_mode() {
        let (tx, rx) = calloop::channel::channel::<DiscordEvent>();
        let v = json!({ "evt": "VOICE_SETTINGS_UPDATE", "data": { "mute": false, "deaf": false } });
        dispatch_event(&v, &tx);
        let e = rx.recv().unwrap();
        match e {
            DiscordEvent::VoiceSettings { mute, deaf } => {
                assert!(!mute);
                assert!(!deaf);
            }
            _ => panic!("expected VoiceSettings"),
        }
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn dispatch_event_voice_settings_update_with_mode() {
        let (tx, rx) = calloop::channel::channel::<DiscordEvent>();
        let v = json!({ "evt": "VOICE_SETTINGS_UPDATE", "data": { "mute": false, "deaf": true, "mode": { "type": "VOICE_ACTIVITY" } } });
        dispatch_event(&v, &tx);
        let e1 = rx.recv().unwrap();
        match e1 {
            DiscordEvent::VoiceSettings { mute, deaf } => {
                assert!(!mute);
                assert!(deaf);
            }
            _ => panic!("expected VoiceSettings"),
        }
        let e2 = rx.recv().unwrap();
        match e2 {
            DiscordEvent::VoiceMode { ptt } => assert!(!ptt),
            _ => panic!("expected VoiceMode"),
        }
    }

    #[test]
    fn dispatch_event_ptt_true() {
        let (tx, rx) = calloop::channel::channel::<DiscordEvent>();
        let v = json!({ "evt": "VOICE_SETTINGS_UPDATE", "data": { "mute": false, "deaf": false, "mode": { "type": "PUSH_TO_TALK" } } });
        dispatch_event(&v, &tx);
        let e1 = rx.recv().unwrap();
        match e1 {
            DiscordEvent::VoiceSettings { mute, deaf } => {
                assert!(!mute);
                assert!(!deaf);
            }
            _ => panic!("expected VoiceSettings"),
        }
        let e2 = rx.recv().unwrap();
        match e2 {
            DiscordEvent::VoiceMode { ptt } => assert!(ptt),
            _ => panic!("expected VoiceMode"),
        }
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn wait_for_ready_reads_ready() {
        let (mut a, mut b) = std::os::unix::net::UnixStream::pair().unwrap();
        let non_ready = json!({"evt": "NOT_READY"});
        super::super::ipc::write_frame(&mut a, super::super::ipc::OP_FRAME, &non_ready.to_string())
            .expect("write");
        let ready = json!({"evt": "READY", "data": { "user": { "id": "u1", "username": "bob" }}});
        super::super::ipc::write_frame(&mut a, super::super::ipc::OP_FRAME, &ready.to_string())
            .expect("write");
        let got = wait_for_ready(&mut b).expect("wait_for_ready");
        assert_eq!(got["evt"].as_str().unwrap(), "READY");
        assert_eq!(got["data"]["user"]["id"].as_str().unwrap(), "u1");
    }

    #[test]
    fn read_frame_invalid_json() {
        use std::io::Write;
        let mut c = Cursor::new(Vec::new());
        let op = 1u32;
        let payload = b"not-json";
        c.write_all(&op.to_le_bytes()).unwrap();
        c.write_all(&(payload.len() as u32).to_le_bytes()).unwrap();
        c.write_all(payload).unwrap();
        c.set_position(0);
        let res = super::super::ipc::read_frame(&mut c);
        assert!(res.is_err());
        assert_eq!(res.unwrap_err().kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn process_frame_get_selected_empty() {
        let v = json!({"cmd": "GET_SELECTED_VOICE_CHANNEL", "nonce": "gvsc", "data": null});
        let (events, avatars, subscribe, guild) = process_frame_events(&v, "local", "me", None);
        assert_eq!(events.len(), 1);
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
        assert!(avatars.is_empty());
        assert!(subscribe.is_none());
        assert!(guild.is_none());
    }

    #[test]
    fn process_frame_get_selected_with_data() {
        let data = json!({
            "id": "chan1",
            "name": "Room",
            "guild_id": "g1",
            "voice_states": [
                { "user": { "id": "local", "username": "me", "avatar": "ahash" }, "voice_state": { "self_mute": false, "self_deaf": false }, "nick": "MeNick" },
                { "user": { "id": "u2", "username": "Bob", "avatar": "bh" }, "voice_state": {}, "nick": "BobNick" }
            ]
        });
        let v = json!({"cmd": "GET_SELECTED_VOICE_CHANNEL", "nonce": "gvsc", "data": data});
        let (events, avatars, subscribe, guild) = process_frame_events(&v, "local", "me", None);
        assert_eq!(subscribe, Some("chan1".to_string()));
        assert_eq!(guild, Some("g1".to_string()));
        assert_eq!(events.len(), 1);
        match &events[0] {
            DiscordEvent::VoiceParticipants {
                participants,
                channel_name,
            } => {
                assert_eq!(participants.len(), 2);
                assert_eq!(channel_name.as_deref(), Some("Room"));
                assert_eq!(participants[0].user_id, "local");
                assert_eq!(
                    participants[0]
                        .nick
                        .as_deref()
                        .unwrap_or(&participants[0].username),
                    "MeNick"
                );
                assert_eq!(participants[1].user_id, "u2");
                assert_eq!(
                    participants[1]
                        .nick
                        .as_deref()
                        .unwrap_or(&participants[1].username),
                    "BobNick"
                );
            }
            _ => panic!("expected VoiceParticipants"),
        }
        assert_eq!(avatars.len(), 2);
    }

    #[test]
    fn process_frame_speaking_start_end() {
        let v1 = json!({"evt": "SPEAKING_START", "data": {"user_id": "u1"}});
        let h1 = super::super::handlers::SpeakingStartHandler;
        let res1 = h1.handle(&v1, "local", "me", None).unwrap();
        assert_eq!(res1.0.len(), 1);

        let v2 = json!({"evt": "SPEAKING_END", "data": {"user_id": "u1"}});
        let h2 = super::super::handlers::SpeakingEndHandler;
        let res2 = h2.handle(&v2, "local", "me", None).unwrap();
        assert_eq!(res2.0.len(), 1);
    }

    #[test]
    fn process_frame_voice_state_update_create_delete() {
        let vs_update = json!({
            "user": {"id": "u2", "username": "frank"},
            "voice_state": {"self_mute": true, "self_deaf": false, "mute": false, "deaf": false}
        });
        let v_update = json!({"evt": "VOICE_STATE_UPDATE", "data": vs_update});
        let h_update = super::super::handlers::VoiceStateUpdateHandler;
        let res_update = h_update.handle(&v_update, "local", "me", None).unwrap();
        assert_eq!(res_update.0.len(), 1);

        let vs_create = json!({
            "user": {"id": "u2", "username": "frank", "avatar": "hash123"},
            "voice_state": {"self_mute": true, "self_deaf": false, "mute": false, "deaf": false}
        });
        let v_create = json!({"evt": "VOICE_STATE_CREATE", "data": vs_create});
        let h_create = super::super::handlers::VoiceStateCreateHandler;
        let res_create = h_create.handle(&v_create, "local", "me", None).unwrap();
        assert_eq!(res_create.0.len(), 1);
        assert_eq!(res_create.1.len(), 1);

        let v_delete = json!({"evt": "VOICE_STATE_DELETE", "data": {"user": {"id": "u3"}}});
        let h_delete = super::super::handlers::VoiceStateDeleteHandler;
        let res_delete = h_delete.handle(&v_delete, "local", "me", None).unwrap();
        assert_eq!(res_delete.0.len(), 1);
    }

    #[test]
    fn process_frame_with_invalid_json() {
        let v = json!({"invalid": "test"});
        let (events, _, _, _) = process_frame_events(&v, "local", "me", None);
        assert!(events.is_empty());
    }
}
