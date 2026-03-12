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

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::mpsc;
use std::time::Duration;

use serde_json::{json, Value};

// ─── Public types ─────────────────────────────────────────────────────────────

pub struct Config {
    pub client_id: String,
    pub client_secret: String,
}

/// A participant in the current voice channel.
#[derive(Debug)]
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
    /// Successfully authenticated; contains the Discord username.
    Ready { username: String },
    /// Current mute/deafen state (sent on connect and on change).
    VoiceSettings { mute: bool, deaf: bool },
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
    /// Connection lost.
    Disconnected,
}

/// Commands sent from the main thread to the Discord thread.
#[derive(Debug)]
pub enum DiscordCommand {
    SetMute(bool),
    SetDeaf(bool),
}

/// Spawn the Discord IPC client in a background thread.
pub fn spawn(
    config: Config,
    tx: calloop::channel::Sender<DiscordEvent>,
    cmd_rx: mpsc::Receiver<DiscordCommand>,
) {
    std::thread::spawn(move || run_client(config, tx, cmd_rx));
}

// ─── Socket location ──────────────────────────────────────────────────────────

fn get_uid() -> u32 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("Uid:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|s| s.parse().ok())
        })
        .unwrap_or(1000)
}

fn find_socket() -> Option<UnixStream> {
    let uid = get_uid();
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| format!("/run/user/{uid}"));
    let tmpdir = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".to_string());

    let bases = [
        runtime.clone(),
        format!("{runtime}/app/com.discordapp.Discord"), // Flatpak
        format!("{runtime}/snap.discord"),               // Snap
        tmpdir,
        "/tmp".to_string(),
    ];

    for base in &bases {
        for i in 0..10 {
            let path = format!("{base}/discord-ipc-{i}");
            if let Ok(s) = UnixStream::connect(&path) {
                eprintln!("[discord] connected to {path}");
                return Some(s);
            }
        }
    }
    None
}

// ─── Frame I/O ────────────────────────────────────────────────────────────────

// Frame: [op: u32 LE][len: u32 LE][payload: UTF-8 JSON]
const OP_HANDSHAKE: u32 = 0;
const OP_FRAME: u32 = 1;

fn write_frame(stream: &mut impl Write, op: u32, payload: &str) -> io::Result<()> {
    let data = payload.as_bytes();
    stream.write_all(&op.to_le_bytes())?;
    stream.write_all(&(data.len() as u32).to_le_bytes())?;
    stream.write_all(data)
}

fn read_frame(stream: &mut impl Read) -> io::Result<(u32, Value)> {
    let mut hdr = [0u8; 8];
    stream.read_exact(&mut hdr)?;
    let op = u32::from_le_bytes(hdr[0..4].try_into().unwrap());
    let len = u32::from_le_bytes(hdr[4..8].try_into().unwrap()) as usize;
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf)?;
    let v =
        serde_json::from_slice(&buf).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok((op, v))
}

fn send_cmd(stream: &mut UnixStream, msg: Value) {
    let _ = write_frame(stream, OP_FRAME, &msg.to_string());
}

// ─── Token cache ─────────────────────────────────────────────────────────────

fn token_path() -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    let dir = std::path::Path::new(&home)
        .join(".cache")
        .join("hypr-overlay");
    let _ = std::fs::create_dir_all(&dir);
    dir.join("discord-token.json")
}

fn load_token() -> Option<(String, String)> {
    let data = std::fs::read_to_string(token_path()).ok()?;
    let v: Value = serde_json::from_str(&data).ok()?;
    Some((
        v["access_token"].as_str()?.to_string(),
        v["refresh_token"].as_str()?.to_string(),
    ))
}

fn save_token(access: &str, refresh: &str) {
    let _ = std::fs::write(
        token_path(),
        json!({"access_token": access, "refresh_token": refresh}).to_string(),
    );
}

// ─── HTTP token exchange ──────────────────────────────────────────────────────

fn http_exchange(params: &str) -> Option<(String, String)> {
    let resp = ureq::post("https://discord.com/api/oauth2/token")
        .set("Content-Type", "application/x-www-form-urlencoded")
        .send_string(params)
        .ok()?;
    let v: Value = resp.into_json().ok()?;
    Some((
        v["access_token"].as_str()?.to_string(),
        v["refresh_token"].as_str()?.to_string(),
    ))
}

fn exchange_code(cfg: &Config, code: &str) -> Option<(String, String)> {
    let params = format!(
        "client_id={}&client_secret={}&grant_type=authorization_code&code={}&redirect_uri=http%3A%2F%2F127.0.0.1",
        cfg.client_id, cfg.client_secret, code
    );
    http_exchange(&params)
}

fn do_refresh(cfg: &Config, refresh: &str) -> Option<(String, String)> {
    let params = format!(
        "client_id={}&client_secret={}&grant_type=refresh_token&refresh_token={}",
        cfg.client_id, cfg.client_secret, refresh
    );
    http_exchange(&params)
}

// ─── Auth helpers ─────────────────────────────────────────────────────────────

/// Distinguishes a token-rejection error from other auth failures.
#[derive(Debug)]
enum AuthError {
    /// Discord rejected the token (evt=ERROR, e.g. code 4009 "Invalid token").
    InvalidToken,
    /// Any other failure (I/O error, timeout, etc.).
    Other,
}

/// Send AUTHENTICATE, wait for response.
fn authenticate(stream: &mut UnixStream, token: &str) -> Result<String, AuthError> {
    send_cmd(
        stream,
        json!({
            "cmd": "AUTHENTICATE",
            "args": { "access_token": token },
            "nonce": "auth"
        }),
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if std::time::Instant::now() > deadline {
            return Err(AuthError::Other);
        }
        match read_frame(stream) {
            Ok((OP_FRAME, v)) if v["nonce"] == "auth" || v["cmd"] == "AUTHENTICATE" => {
                if v["evt"] == "ERROR" {
                    eprintln!("[discord] AUTHENTICATE error: {}", v["data"]["message"]);
                    return Err(AuthError::InvalidToken);
                }
                return Ok(token.to_string());
            }
            Ok(_) => continue,
            Err(e) if is_timeout(&e) => continue,
            Err(_) => return Err(AuthError::Other),
        }
    }
}

/// Full OAuth flow: AUTHORIZE → code → HTTP exchange → AUTHENTICATE.
fn authorize_flow(cfg: &Config, stream: &mut UnixStream) -> Option<String> {
    eprintln!("[discord] Starting OAuth — please check Discord for an authorization popup");
    send_cmd(
        stream,
        json!({
            "cmd":  "AUTHORIZE",
            "args": {
                "client_id": cfg.client_id,
                "scopes":    ["rpc", "rpc.voice.read", "rpc.voice.write"]
            },
            "nonce": "authorize"
        }),
    );

    // Wait up to 2 minutes for the user to click "Authorize" in Discord
    let deadline = std::time::Instant::now() + Duration::from_secs(120);
    let code = loop {
        if std::time::Instant::now() > deadline {
            eprintln!("[discord] Authorization timed out");
            return None;
        }
        match read_frame(stream) {
            Ok((OP_FRAME, v)) if v["nonce"] == "authorize" || v["cmd"] == "AUTHORIZE" => {
                if v["evt"] == "ERROR" {
                    eprintln!("[discord] AUTHORIZE error: {}", v["data"]["message"]);
                    return None;
                }
                if let Some(code) = v["data"]["code"].as_str() {
                    break code.to_string();
                }
            }
            Ok(_) => continue,
            Err(e) if is_timeout(&e) => continue,
            Err(e) => {
                eprintln!("[discord] error during AUTHORIZE: {e}");
                return None;
            }
        }
    };

    eprintln!("[discord] Got auth code, exchanging for access token...");
    let (at, rt) = exchange_code(cfg, &code)?;
    save_token(&at, &rt);
    eprintln!("[discord] Token saved to {}", token_path().display());
    authenticate(stream, &at).ok()
}

fn try_auth(cfg: &Config, stream: &mut UnixStream) -> Option<String> {
    if let Some((at, rt)) = load_token() {
        // Try the cached access token
        match authenticate(stream, &at) {
            Ok(tok) => return Some(tok),
            Err(AuthError::InvalidToken) => {
                // Token invalid/expired — try the refresh token
                eprintln!("[discord] Access token invalid/expired, refreshing...");
                if let Some((new_at, new_rt)) = do_refresh(cfg, &rt) {
                    save_token(&new_at, &new_rt);
                    match authenticate(stream, &new_at) {
                        Ok(tok) => return Some(tok),
                        Err(AuthError::InvalidToken) => {
                            // Refresh token also rejected — clear cache and do full OAuth
                            eprintln!(
                                "[discord] Refresh token invalid, clearing cache and re-authenticating"
                            );
                            let _ = std::fs::remove_file(token_path());
                        }
                        Err(AuthError::Other) => return None,
                    }
                } else {
                    // HTTP refresh request failed — clear stale cache and do full OAuth
                    eprintln!("[discord] Token refresh failed, clearing cache");
                    let _ = std::fs::remove_file(token_path());
                }
            }
            Err(AuthError::Other) => return None,
        }
    }
    // No valid token — do full OAuth flow
    authorize_flow(cfg, stream)
}

// ─── Voice helpers ────────────────────────────────────────────────────────────

fn parse_participants(channel_data: &serde_json::Value) -> Vec<Participant> {
    let states = channel_data["voice_states"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    states.iter().map(|vs| parse_voice_state(vs)).collect()
}

fn parse_voice_state(vs: &serde_json::Value) -> Participant {
    let user = &vs["user"];
    let vs_inner = &vs["voice_state"];
    let self_mute = vs_inner["self_mute"].as_bool().unwrap_or(false);
    let self_deaf = vs_inner["self_deaf"].as_bool().unwrap_or(false);
    let server_mute =
        vs["mute"].as_bool().unwrap_or(false) || vs_inner["mute"].as_bool().unwrap_or(false);
    let server_deaf = vs_inner["deaf"].as_bool().unwrap_or(false);
    Participant {
        user_id: user["id"].as_str().unwrap_or("").to_string(),
        username: user["username"].as_str().unwrap_or("?").to_string(),
        nick: vs["nick"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string()),
        avatar_hash: user["avatar"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string()),
        muted: self_mute || server_mute,
        deafened: self_deaf || server_deaf,
    }
}

fn subscribe_for_channel(stream: &mut UnixStream, channel_id: &str, nonce: &mut u64) {
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

fn fetch_and_send_avatar(
    user_id: String,
    hash: String,
    tx: calloop::channel::Sender<DiscordEvent>,
) {
    std::thread::spawn(move || {
        let url = format!(
            "https://cdn.discordapp.com/avatars/{}/{}.png?size=64",
            user_id, hash
        );
        if let Ok(resp) = ureq::get(&url).call() {
            let mut buf = Vec::new();
            if resp.into_reader().read_to_end(&mut buf).is_ok() {
                if let Ok(img) = image::load_from_memory(&buf) {
                    let rgba = img.to_rgba8();
                    let (w, _h) = rgba.dimensions();
                    let size = w;
                    if size > 0 {
                        let _ = tx.send(DiscordEvent::AvatarLoaded {
                            user_id,
                            rgba: rgba.into_raw(),
                            size,
                        });
                    }
                }
            }
        }
    });
}

// ─── Main IPC loop ────────────────────────────────────────────────────────────

fn run_client(
    cfg: Config,
    tx: calloop::channel::Sender<DiscordEvent>,
    cmd_rx: mpsc::Receiver<DiscordCommand>,
) {
    let mut backoff_secs = 1u64;
    loop {
        eprintln!("[discord] connecting...");
        match try_connect(&cfg, &tx, &cmd_rx) {
            Ok(()) => {
                eprintln!("[discord] disconnected cleanly, reconnecting...");
                backoff_secs = 1;
            }
            Err(()) => {
                eprintln!("[discord] connection error, retrying in {backoff_secs}s");
            }
        }
        // Notify UI so it can reset to the idle/disconnected state.
        let _ = tx.send(DiscordEvent::Disconnected);
        std::thread::sleep(Duration::from_secs(backoff_secs));
        backoff_secs = (backoff_secs * 2).min(30);
    }
}

fn try_connect(
    cfg: &Config,
    tx: &calloop::channel::Sender<DiscordEvent>,
    cmd_rx: &mpsc::Receiver<DiscordCommand>,
) -> Result<(), ()> {
    // Find Discord socket
    let mut stream = loop {
        match find_socket() {
            Some(s) => break s,
            None => {
                eprintln!("[discord] Discord not found, retrying in 5s...");
                std::thread::sleep(Duration::from_secs(5));
            }
        }
    };

    // Read events with 50ms timeout so we can also check for commands
    stream
        .set_read_timeout(Some(Duration::from_millis(50)))
        .ok();

    // Handshake
    let handshake = json!({"v": 1, "client_id": cfg.client_id}).to_string();
    write_frame(&mut stream, OP_HANDSHAKE, &handshake).map_err(|_| ())?;

    // Wait for READY — capture local user info to prepend to participant lists
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
    eprintln!("[discord] handshake OK — user: {local_username} ({local_user_id})");

    // Authenticate
    try_auth(cfg, &mut stream).ok_or(())?;
    eprintln!("[discord] authenticated");
    let _ = tx.send(DiscordEvent::Ready {
        username: local_username.clone(),
    });

    // Get current voice settings + subscribe to updates
    send_cmd(
        &mut stream,
        json!({
            "cmd": "GET_VOICE_SETTINGS",
            "args": {},
            "nonce": "gvs"
        }),
    );
    send_cmd(
        &mut stream,
        json!({
            "cmd":  "SUBSCRIBE",
            "evt":  "VOICE_SETTINGS_UPDATE",
            "args": {},
            "nonce": "sub_vsu"
        }),
    );
    // Subscribe to voice channel changes and get current voice channel
    send_cmd(
        &mut stream,
        json!({
            "cmd": "SUBSCRIBE",
            "evt": "VOICE_CHANNEL_SELECT",
            "args": {},
            "nonce": "sub_vcs"
        }),
    );
    send_cmd(
        &mut stream,
        json!({
            "cmd": "GET_SELECTED_VOICE_CHANNEL",
            "args": {},
            "nonce": "gvsc"
        }),
    );

    // Event loop
    let mut nonce: u64 = 1000;
    loop {
        // Handle commands from main thread
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

        // Read next event (may time out)
        match read_frame(&mut stream) {
            Ok((OP_FRAME, v)) => {
                let cmd = v["cmd"].as_str().unwrap_or("");
                let evt = v["evt"].as_str().unwrap_or("");
                let vnonce = v["nonce"].as_str().unwrap_or("");
                eprintln!("[discord] frame cmd={cmd:?} evt={evt:?} nonce={vnonce:?}");

                // GET_SELECTED_VOICE_CHANNEL response (match by cmd+nonce)
                if cmd == "GET_SELECTED_VOICE_CHANNEL" && vnonce == "gvsc" {
                    eprintln!("[discord] gvsc data: {}", v["data"]);
                    if !v["data"].is_null() {
                        let cid = v["data"]["id"].as_str().unwrap_or("").to_string();
                        if !cid.is_empty() {
                            eprintln!("[discord] subscribing SPEAKING_START for channel {cid}");
                            subscribe_for_channel(&mut stream, &cid, &mut nonce);
                        }
                        // Build participant list — self first, then others (skip self if already in voice_states)
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
                        let mut parts = vec![Participant {
                            user_id: local_user_id.clone(),
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
                        eprintln!(
                            "[discord] {} participant(s) in channel {:?}",
                            parts.len(),
                            channel_name
                        );
                        for p in &parts {
                            if let Some(hash) = &p.avatar_hash {
                                fetch_and_send_avatar(p.user_id.clone(), hash.clone(), tx.clone());
                            }
                        }
                        let _ = tx.send(DiscordEvent::VoiceParticipants {
                            participants: parts,
                            channel_name,
                        });
                    } else {
                        eprintln!("[discord] not in a voice channel");
                        let _ = tx.send(DiscordEvent::VoiceParticipants {
                            participants: vec![],
                            channel_name: None,
                        });
                    }
                } else if evt == "VOICE_CHANNEL_SELECT" {
                    let cid = v["data"]["channel_id"].as_str().unwrap_or("");
                    eprintln!("[discord] VOICE_CHANNEL_SELECT channel_id={cid:?}");
                    if !cid.is_empty() {
                        // Subscribe AFTER confirming channel via GET_SELECTED_VOICE_CHANNEL
                        send_cmd(
                            &mut stream,
                            json!({
                                "cmd": "GET_SELECTED_VOICE_CHANNEL",
                                "args": {},
                                "nonce": "gvsc"
                            }),
                        );
                    } else {
                        let _ = tx.send(DiscordEvent::VoiceParticipants {
                            participants: vec![],
                            channel_name: None,
                        });
                    }
                } else if evt == "SPEAKING_START" {
                    if let Some(uid) = v["data"]["user_id"].as_str() {
                        eprintln!("[discord] speaking_start user_id={uid}");
                        let _ = tx.send(DiscordEvent::SpeakingUpdate {
                            user_id: uid.to_string(),
                            speaking: true,
                        });
                    }
                } else if evt == "SPEAKING_END" {
                    if let Some(uid) = v["data"]["user_id"].as_str() {
                        eprintln!("[discord] speaking_end user_id={uid}");
                        let _ = tx.send(DiscordEvent::SpeakingUpdate {
                            user_id: uid.to_string(),
                            speaking: false,
                        });
                    }
                } else if evt == "VOICE_STATE_UPDATE" {
                    eprintln!("[discord] VOICE_STATE_UPDATE data={}", v["data"]);
                    let p = parse_voice_state(&v["data"]);
                    if !p.user_id.is_empty() {
                        let _ = tx.send(DiscordEvent::ParticipantStateUpdate {
                            user_id: p.user_id,
                            muted: p.muted,
                            deafened: p.deafened,
                        });
                    }
                } else if evt == "VOICE_STATE_CREATE" {
                    eprintln!("[discord] VOICE_STATE_CREATE data={}", v["data"]);
                    let p = parse_voice_state(&v["data"]);
                    if !p.user_id.is_empty() {
                        if let Some(hash) = &p.avatar_hash {
                            fetch_and_send_avatar(p.user_id.clone(), hash.clone(), tx.clone());
                        }
                        let _ = tx.send(DiscordEvent::UserJoined(p));
                    }
                } else if evt == "VOICE_STATE_DELETE" {
                    eprintln!("[discord] VOICE_STATE_DELETE data={}", v["data"]);
                    if let Some(uid) = v["data"]["user"]["id"].as_str() {
                        let _ = tx.send(DiscordEvent::UserLeft {
                            user_id: uid.to_string(),
                        });
                    }
                } else if evt == "SPEAKING_END" {
                    if let Some(uid) = v["data"]["user_id"].as_str() {
                        eprintln!("[discord] speaking_end user_id={uid}");
                        let _ = tx.send(DiscordEvent::SpeakingUpdate {
                            user_id: uid.to_string(),
                            speaking: false,
                        });
                    }
                } else if evt == "ERROR" {
                    eprintln!(
                        "[discord] ERROR cmd={cmd:?} nonce={vnonce:?} msg={}",
                        v["data"]["message"]
                    );
                } else {
                    dispatch_event(&v, tx);
                }
            }
            Ok(_) => {}
            Err(e) if is_timeout(&e) => {} // normal 50ms timeout
            Err(e) => {
                eprintln!("[discord] read error: {e}");
                // We were authenticated; treat as a clean disconnect so the
                // reconnect loop resets the backoff to 1 s.
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

fn dispatch_event(v: &Value, tx: &calloop::channel::Sender<DiscordEvent>) {
    match v["evt"].as_str() {
        Some("VOICE_SETTINGS_UPDATE") => {
            let mute = v["data"]["mute"].as_bool().unwrap_or(false);
            let deaf = v["data"]["deaf"].as_bool().unwrap_or(false);
            let _ = tx.send(DiscordEvent::VoiceSettings { mute, deaf });
        }
        _ => {
            // Also handle GET_VOICE_SETTINGS response
            if v["cmd"] == "GET_VOICE_SETTINGS" && v["nonce"] == "gvs" {
                let mute = v["data"]["mute"].as_bool().unwrap_or(false);
                let deaf = v["data"]["deaf"].as_bool().unwrap_or(false);
                let _ = tx.send(DiscordEvent::VoiceSettings { mute, deaf });
            }
        }
    }
}

fn is_timeout(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}
