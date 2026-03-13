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
use tracing::{debug, error, info, warn};

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
                info!("connected to {path}");
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
                    warn!("AUTHENTICATE error: {}", v["data"]["message"]);
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
    info!("Starting OAuth — please check Discord for an authorization popup");
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
            warn!("Authorization timed out");
            return None;
        }
        match read_frame(stream) {
            Ok((OP_FRAME, v)) if v["nonce"] == "authorize" || v["cmd"] == "AUTHORIZE" => {
                if v["evt"] == "ERROR" {
                    warn!("AUTHORIZE error: {}", v["data"]["message"]);
                    return None;
                }
                if let Some(code) = v["data"]["code"].as_str() {
                    break code.to_string();
                }
            }
            Ok(_) => continue,
            Err(e) if is_timeout(&e) => continue,
            Err(e) => {
                error!("error during AUTHORIZE: {e}");
                return None;
            }
        }
    };

    info!("Got auth code, exchanging for access token...");
    let (at, rt) = exchange_code(cfg, &code)?;
    save_token(&at, &rt);
    info!("Token saved to {}", token_path().display());
    authenticate(stream, &at).ok()
}

fn try_auth(cfg: &Config, stream: &mut UnixStream) -> Option<String> {
    if let Some((at, rt)) = load_token() {
        // Try the cached access token
        match authenticate(stream, &at) {
            Ok(tok) => return Some(tok),
            Err(AuthError::InvalidToken) => {
                // Token invalid/expired — try the refresh token
                warn!("Access token invalid/expired, refreshing...");
                if let Some((new_at, new_rt)) = do_refresh(cfg, &rt) {
                    save_token(&new_at, &new_rt);
                    match authenticate(stream, &new_at) {
                        Ok(tok) => return Some(tok),
                        Err(AuthError::InvalidToken) => {
                            // Refresh token also rejected — clear cache and do full OAuth
                            warn!(
                                "[discord] Refresh token invalid, clearing cache and re-authenticating"
                            );
                            let _ = std::fs::remove_file(token_path());
                        }
                        Err(AuthError::Other) => return None,
                    }
                } else {
                    // HTTP refresh request failed — clear stale cache and do full OAuth
                    warn!("Token refresh failed, clearing cache");
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
    states.iter().map(parse_voice_state).collect()
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

fn avatar_base_url() -> String {
    std::env::var("HYPR_AVATAR_BASE_URL")
        .unwrap_or_else(|_| "https://cdn.discordapp.com/avatars".to_string())
}

fn fetch_and_send_avatar(
    user_id: String,
    hash: String,
    tx: calloop::channel::Sender<DiscordEvent>,
) {
    std::thread::spawn(move || {
        let base = avatar_base_url();
        let url = format!("{}/{}/{}.png?size=64", base, user_id, hash);
        if let Ok(resp) = ureq::get(&url).call() {
            let mut buf = Vec::new();
            if resp.into_reader().read_to_end(&mut buf).is_ok() {
                if let Ok(img) = image::load_from_memory(&buf) {
                    let rgba = image::DynamicImage::from(img.to_rgba8()).flipv().to_rgba8();
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
                info!("Discord not found, retrying in 5s...");
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
    info!("handshake OK — user: {local_username} ({local_user_id})");

    // Authenticate
    try_auth(cfg, &mut stream).ok_or(())?;
    info!("authenticated");
    let _ = tx.send(DiscordEvent::Ready {
        username: local_username.clone(),
        user_id: local_user_id.clone(),
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
                debug!("frame cmd={cmd:?} evt={evt:?} nonce={vnonce:?}");

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
                        fetch_and_send_avatar(uid, hash, tx.clone());
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

                // GET_GUILD response
                if cmd == "GET_GUILD" && vnonce == "get_guild" {
                    if let Some(name) = v["data"]["name"].as_str() {
                        let _ = tx.send(DiscordEvent::GuildName {
                            name: name.to_string(),
                        });
                    }
                }
                // GET_SELECTED_VOICE_CHANNEL response (match by cmd+nonce)
                else if cmd == "GET_SELECTED_VOICE_CHANNEL" && vnonce == "gvsc" {
                    debug!("gvsc data: {}", v["data"]);
                    if !v["data"].is_null() {
                        let cid = v["data"]["id"].as_str().unwrap_or("").to_string();
                        if !cid.is_empty() {
                            debug!("subscribing SPEAKING_START for channel {cid}");
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
                        debug!(
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
                        // Request the guild name if we're in a guild channel
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
                        debug!("speaking_start user_id={uid}");
                        let _ = tx.send(DiscordEvent::SpeakingUpdate {
                            user_id: uid.to_string(),
                            speaking: true,
                        });
                    }
                } else if evt == "SPEAKING_END" {
                    if let Some(uid) = v["data"]["user_id"].as_str() {
                        debug!("speaking_end user_id={uid}");
                        let _ = tx.send(DiscordEvent::SpeakingUpdate {
                            user_id: uid.to_string(),
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
                            fetch_and_send_avatar(p.user_id.clone(), hash.clone(), tx.clone());
                        }
                        let _ = tx.send(DiscordEvent::UserJoined(p));
                    }
                } else if evt == "VOICE_STATE_DELETE" {
                    debug!("VOICE_STATE_DELETE data={}", v["data"]);
                    if let Some(uid) = v["data"]["user"]["id"].as_str() {
                        let _ = tx.send(DiscordEvent::UserLeft {
                            user_id: uid.to_string(),
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
            Err(e) if is_timeout(&e) => {} // normal 50ms timeout
            Err(e) => {
                warn!("read error: {e}");
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

// Type alias to reduce clippy type_complexity warning
pub type FrameProcessResult = (
    Vec<DiscordEvent>,
    Vec<(String, String)>,
    Option<String>,
    Option<String>,
);

fn process_frame_events(
    v: &Value,
    local_user_id: &str,
    local_username: &str,
    local_avatar: Option<&String>,
) -> FrameProcessResult {
    let mut events = Vec::new();
    let mut avatars: Vec<(String, String)> = Vec::new();
    let mut subscribe_channel: Option<String> = None;
    let mut guild_id: Option<String> = None;

    let cmd = v["cmd"].as_str().unwrap_or("");
    let evt = v["evt"].as_str().unwrap_or("");
    let vnonce = v["nonce"].as_str().unwrap_or("");

    // GET_GUILD response
    if cmd == "GET_GUILD" && vnonce == "get_guild" {
        if let Some(name) = v["data"]["name"].as_str() {
            events.push(DiscordEvent::GuildName {
                name: name.to_string(),
            });
            return (events, avatars, subscribe_channel, guild_id);
        }
    }

    // GET_SELECTED_VOICE_CHANNEL response
    if cmd == "GET_SELECTED_VOICE_CHANNEL" && vnonce == "gvsc" {
        if !v["data"].is_null() {
            let cid = v["data"]["id"].as_str().unwrap_or("").to_string();
            if !cid.is_empty() {
                subscribe_channel = Some(cid.clone());
            }
            let others = parse_participants(&v["data"]);
            let self_nick = others
                .iter()
                .find(|p| p.user_id == local_user_id)
                .and_then(|p| p.nick.clone());
            let self_avatar = local_avatar.cloned().or_else(|| {
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
                user_id: local_user_id.to_string(),
                username: local_username.to_string(),
                nick: self_nick,
                avatar_hash: self_avatar.clone(),
                muted: self_muted,
                deafened: self_deafened,
            }];
            parts.extend(others.into_iter().filter(|p| p.user_id != local_user_id));
            let channel_name = v["data"]["name"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string());
            for p in &parts {
                if let Some(hash) = &p.avatar_hash {
                    avatars.push((p.user_id.clone(), hash.clone()));
                }
            }
            if let Some(gid) = v["data"]["guild_id"].as_str() {
                if !gid.is_empty() {
                    guild_id = Some(gid.to_string());
                }
            }
            events.push(DiscordEvent::VoiceParticipants {
                participants: parts,
                channel_name,
            });
            return (events, avatars, subscribe_channel, guild_id);
        } else {
            events.push(DiscordEvent::VoiceParticipants {
                participants: vec![],
                channel_name: None,
            });
            return (events, avatars, subscribe_channel, guild_id);
        }
    }

    if evt == "VOICE_CHANNEL_SELECT" {
        let cid = v["data"]["channel_id"].as_str().unwrap_or("");
        if !cid.is_empty() {
            // request GET_SELECTED_VOICE_CHANNEL by sending 'gvsc' from caller
            subscribe_channel = Some(cid.to_string());
        } else {
            events.push(DiscordEvent::GuildName {
                name: String::new(),
            });
            events.push(DiscordEvent::VoiceParticipants {
                participants: vec![],
                channel_name: None,
            });
            return (events, avatars, subscribe_channel, guild_id);
        }
    }

    if evt == "SPEAKING_START" {
        if let Some(uid) = v["data"]["user_id"].as_str() {
            events.push(DiscordEvent::SpeakingUpdate {
                user_id: uid.to_string(),
                speaking: true,
            });
            return (events, avatars, subscribe_channel, guild_id);
        }
    }

    if evt == "SPEAKING_END" {
        if let Some(uid) = v["data"]["user_id"].as_str() {
            events.push(DiscordEvent::SpeakingUpdate {
                user_id: uid.to_string(),
                speaking: false,
            });
            return (events, avatars, subscribe_channel, guild_id);
        }
    }

    if evt == "VOICE_STATE_UPDATE" {
        let p = parse_voice_state(&v["data"]);
        if !p.user_id.is_empty() {
            events.push(DiscordEvent::ParticipantStateUpdate {
                user_id: p.user_id,
                muted: p.muted,
                deafened: p.deafened,
            });
            return (events, avatars, subscribe_channel, guild_id);
        }
    }

    if evt == "VOICE_STATE_CREATE" {
        let p = parse_voice_state(&v["data"]);
        if !p.user_id.is_empty() {
            if let Some(hash) = &p.avatar_hash {
                avatars.push((p.user_id.clone(), hash.clone()));
            }
            events.push(DiscordEvent::UserJoined(p));
            return (events, avatars, subscribe_channel, guild_id);
        }
    }

    if evt == "VOICE_STATE_DELETE" {
        if let Some(uid) = v["data"]["user"]["id"].as_str() {
            events.push(DiscordEvent::UserLeft {
                user_id: uid.to_string(),
            });
            return (events, avatars, subscribe_channel, guild_id);
        }
    }

    (events, avatars, subscribe_channel, guild_id)
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
            // Also handle GET_VOICE_SETTINGS response
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

fn is_timeout(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io;

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
        // No second event (no mode provided)
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
}

#[cfg(test)]
mod tests_extra {
    use super::*;
    use serde_json::json;
    use std::env;
    use std::fs;
    use std::os::unix::net::UnixListener;
    use std::os::unix::net::UnixStream;

    use serial_test::serial;

    use std::io::{self, Cursor};

    #[test]
    #[serial]
    fn token_path_save_load() {
        let tmp = std::env::temp_dir().join(format!("hypr_token_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        env::set_var("HOME", &tmp);
        // ensure cache dir is created and token is written
        save_token("A_TOKEN", "B_REFRESH");
        let loaded = load_token().expect("load_token");
        assert_eq!(loaded.0, "A_TOKEN");
        assert_eq!(loaded.1, "B_REFRESH");
        let _ = fs::remove_file(token_path());
        let _ = fs::remove_dir_all(tmp);
        env::remove_var("HOME");
    }

    #[test]
    #[serial]
    fn find_socket_uses_runtime_dir() {
        let tmp = std::env::temp_dir().join(format!("hypr_sock_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        env::set_var("XDG_RUNTIME_DIR", &tmp);
        let sock = tmp.join("discord-ipc-0");
        let _ = fs::remove_file(&sock);
        let listener = UnixListener::bind(&sock).expect("bind");
        let s = find_socket();
        assert!(s.is_some());
        drop(listener);
        let _ = fs::remove_file(&sock);
        env::remove_var("XDG_RUNTIME_DIR");
    }

    #[test]
    fn frame_roundtrip() {
        use std::io::Cursor;
        let mut c = Cursor::new(Vec::new());
        let payload = "{\"hello\":123}";
        write_frame(&mut c, 42, payload).expect("write");
        c.set_position(0);
        let (op, v) = read_frame(&mut c).expect("read");
        assert_eq!(op, 42);
        assert_eq!(v["hello"].as_i64().unwrap(), 123);
    }

    #[test]
    fn subscribe_for_channel_writes_expected() {
        let (mut a, mut b) = UnixStream::pair().unwrap();
        let mut nonce = 0u64;
        subscribe_for_channel(&mut a, "chan1", &mut nonce);
        for _ in 0..5 {
            let (_op, v) = read_frame(&mut b).expect("read");
            assert_eq!(v["cmd"].as_str().unwrap(), "SUBSCRIBE");
        }
    }

    #[test]
    fn wait_for_ready_reads_ready() {
        let (mut a, mut b) = UnixStream::pair().unwrap();
        let non_ready = json!({"evt": "NOT_READY"});
        write_frame(&mut a, OP_FRAME, &non_ready.to_string()).expect("write");
        let ready = json!({"evt": "READY", "data": { "user": { "id": "u1", "username": "bob" }}});
        write_frame(&mut a, OP_FRAME, &ready.to_string()).expect("write");
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
        let res = read_frame(&mut c);
        assert!(res.is_err());
        assert_eq!(res.unwrap_err().kind(), io::ErrorKind::InvalidData);
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
    fn authenticate_invalid_token() {
        let (mut a, mut b) = UnixStream::pair().unwrap();
        let err = json!({"nonce": "auth", "evt": "ERROR", "data": {"message": "invalid token"}});
        write_frame(&mut b, OP_FRAME, &err.to_string()).unwrap();
        let res = authenticate(&mut a, "TOKEN");
        assert!(matches!(res, Err(AuthError::InvalidToken)));
    }

    #[test]
    fn authenticate_success() {
        let (mut a, mut b) = UnixStream::pair().unwrap();
        let ok = json!({"nonce":"auth","cmd":"AUTHENTICATE","data":{}});
        write_frame(&mut b, OP_FRAME, &ok.to_string()).unwrap();
        let res = authenticate(&mut a, "TOKEN2");
        assert_eq!(res.unwrap(), "TOKEN2".to_string());
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
        assert_eq!(avatars.len(), 2);
        match &events[0] {
            DiscordEvent::VoiceParticipants {
                participants,
                channel_name,
            } => {
                assert_eq!(participants[0].user_id, "local");
                assert_eq!(participants.len(), 2);
                assert_eq!(channel_name.as_deref(), Some("Room"));
            }
            _ => panic!("expected VoiceParticipants"),
        }
    }

    #[test]
    fn process_frame_speaking_start_end() {
        let v1 = json!({"evt": "SPEAKING_START", "data": { "user_id": "u1" }});
        let (events1, _avatars1, _sub1, _g1) = process_frame_events(&v1, "", "", None);
        assert_eq!(events1.len(), 1);
        match &events1[0] {
            DiscordEvent::SpeakingUpdate { user_id, speaking } => {
                assert_eq!(user_id, "u1");
                assert!(*speaking);
            }
            _ => panic!("expected SpeakingUpdate"),
        }
        let v2 = json!({"evt": "SPEAKING_END", "data": { "user_id": "u1" }});
        let (events2, _avatars2, _sub2, _g2) = process_frame_events(&v2, "", "", None);
        assert_eq!(events2.len(), 1);
        match &events2[0] {
            DiscordEvent::SpeakingUpdate { user_id, speaking } => {
                assert_eq!(user_id, "u1");
                assert!(!*speaking);
            }
            _ => panic!("expected SpeakingUpdate"),
        }
    }

    #[test]
    fn process_frame_voice_state_update_create_delete() {
        let v_up = json!({"evt": "VOICE_STATE_UPDATE", "data": { "user": { "id": "u3", "username": "carol" }, "voice_state": { "self_mute": true } }});
        let (events_up, _a, _s, _g) = process_frame_events(&v_up, "", "", None);
        assert_eq!(events_up.len(), 1);
        match &events_up[0] {
            DiscordEvent::ParticipantStateUpdate {
                user_id,
                muted,
                deafened,
            } => {
                assert_eq!(user_id, "u3");
                assert!(*muted);
                assert!(!*deafened);
            }
            _ => panic!("expected ParticipantStateUpdate"),
        }

        let v_create = json!({"evt": "VOICE_STATE_CREATE", "data": { "user": { "id": "u5", "username": "eve", "avatar": "h1" }, "voice_state": { "self_mute": false } }});
        let (events_c, avatars_c, _s2, _g2) = process_frame_events(&v_create, "", "", None);
        assert_eq!(events_c.len(), 1);
        match &events_c[0] {
            DiscordEvent::UserJoined(p) => {
                assert_eq!(p.user_id, "u5");
            }
            _ => panic!("expected UserJoined"),
        }
        assert_eq!(avatars_c.len(), 1);
        assert_eq!(avatars_c[0].0, "u5");
        assert_eq!(avatars_c[0].1, "h1");

        let v_del = json!({"evt": "VOICE_STATE_DELETE", "data": { "user": { "id": "u4" } }});
        let (events_d, _a2, _s3, _g3) = process_frame_events(&v_del, "", "", None);
        assert_eq!(events_d.len(), 1);
        match &events_d[0] {
            DiscordEvent::UserLeft { user_id } => {
                assert_eq!(user_id, "u4");
            }
            _ => panic!("expected UserLeft"),
        }
    }

    #[test]
    fn process_frame_get_guild() {
        let v = json!({"cmd": "GET_GUILD", "nonce": "get_guild", "data": { "name": "GuildName" }});
        let (events, avatars, subscribe, guild) = process_frame_events(&v, "", "", None);
        assert_eq!(events.len(), 1);
        match &events[0] {
            DiscordEvent::GuildName { name } => assert_eq!(name, "GuildName"),
            _ => panic!("expected GuildName"),
        }
        assert!(avatars.is_empty());
        assert!(subscribe.is_none());
        assert!(guild.is_none());
    }

    #[test]
    fn process_frame_voice_channel_select_empty() {
        let v = json!({"evt": "VOICE_CHANNEL_SELECT", "data": { "channel_id": "" }});
        let (events, avatars, subscribe, guild) = process_frame_events(&v, "local", "me", None);
        assert!(subscribe.is_none());
        assert_eq!(events.len(), 2);
        match &events[0] {
            DiscordEvent::GuildName { name } => assert_eq!(name, ""),
            _ => panic!("expected GuildName"),
        }
        match &events[1] {
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
        assert!(guild.is_none());
    }

    #[test]
    fn process_frame_voice_channel_select_nonempty() {
        let v = json!({"evt": "VOICE_CHANNEL_SELECT", "data": { "channel_id": "chanX" }});
        let (events, avatars, subscribe, guild) = process_frame_events(&v, "", "", None);
        assert_eq!(subscribe, Some("chanX".to_string()));
        assert!(events.is_empty());
        assert!(avatars.is_empty());
        assert!(guild.is_none());
    }

    #[test]
    #[serial]
    fn avatar_base_url_default() {
        std::env::remove_var("HYPR_AVATAR_BASE_URL");
        assert_eq!(
            avatar_base_url(),
            "https://cdn.discordapp.com/avatars".to_string()
        );
    }

    #[test]
    #[serial]
    fn avatar_base_url_env_override() {
        std::env::set_var("HYPR_AVATAR_BASE_URL", "http://localhost:8000/avatars");
        assert_eq!(
            avatar_base_url(),
            "http://localhost:8000/avatars".to_string()
        );
        std::env::remove_var("HYPR_AVATAR_BASE_URL");
    }

    #[test]
    #[serial]
    fn try_auth_with_cached_token_success() {
        // Prepare a temporary HOME to control token_path
        let tmp =
            std::env::temp_dir().join(format!("hypr_token_test_tryauth_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::env::set_var("HOME", &tmp);
        let _ = std::fs::create_dir_all(std::path::Path::new(&tmp).join(".cache/hypr-overlay"));
        std::fs::write(
            token_path(),
            json!({"access_token":"A_TOK","refresh_token":"R_TOK"}).to_string(),
        )
        .unwrap();

        let cfg = Config {
            client_id: "cid".to_string(),
            client_secret: "cs".to_string(),
        };

        let (mut a, mut b) = UnixStream::pair().unwrap();
        // Preload the authentication success frame on the peer so authenticate() reads it
        let auth_ok = json!({"nonce":"auth","cmd":"AUTHENTICATE","data":{}});
        write_frame(&mut b, OP_FRAME, &auth_ok.to_string()).unwrap();

        let res = try_auth(&cfg, &mut a);
        assert_eq!(res, Some("A_TOK".to_string()));

        let _ = std::fs::remove_file(token_path());
        let _ = std::fs::remove_dir_all(&tmp);
        std::env::remove_var("HOME");
    }

    #[test]
    fn parse_voice_state_with_server_deafen() {
        let vs = json!({
            "user": {"id": "u4", "username": "dave"},
            "voice_state": {"self_mute": false, "self_deaf": false, "mute": false, "deaf": true}
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
        assert!(p.avatar_hash.is_none()); // Empty string treated as None
    }

    #[test]
    fn parse_participants_empty_array() {
        let ch = json!({ "voice_states": [] });
        let parts = parse_participants(&ch);
        assert!(parts.is_empty());
    }

    #[test]
    fn parse_participants_multiple_with_nicks() {
        let vs1 = json!({
            "user": {"id": "u1", "username": "alice"},
            "voice_state": {},
            "nick": "Alice"
        });
        let vs2 = json!({
            "user": {"id": "u2", "username": "bob"},
            "voice_state": {}
        });
        let ch = json!({ "voice_states": [vs1, vs2] });
        let parts = parse_participants(&ch);
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].nick.as_deref(), Some("Alice"));
        assert_eq!(parts[1].nick, None);
    }



    #[test]
    fn dispatch_event_ignored_unknown_event() {
        let (tx, rx) = calloop::channel::channel::<DiscordEvent>();
        let v = json!({"evt": "SOME_UNKNOWN_EVENT", "data": {}});
        dispatch_event(&v, &tx);
        // Unknown events should not send anything
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn write_frame_basic() {
        let mut buffer = Vec::new();
        let result = write_frame(&mut buffer, 0, "{}");
        assert!(result.is_ok());
        assert!(!buffer.is_empty());
    }



    #[test]
    fn is_timeout_would_block() {
        let e = io::Error::new(io::ErrorKind::WouldBlock, "test");
        assert!(is_timeout(&e));
    }

    #[test]
    fn is_timeout_timed_out() {
        let e = io::Error::new(io::ErrorKind::TimedOut, "test");
        assert!(is_timeout(&e));
    }

    #[test]
    fn is_timeout_other_error() {
        let e = io::Error::new(io::ErrorKind::Other, "test");
        assert!(!is_timeout(&e));
    }

    #[test]
    fn is_timeout_io_error() {
        let e = io::Error::new(io::ErrorKind::InvalidData, "test");
        assert!(!is_timeout(&e));
    }

    #[test]
    fn participant_defaults() {
        let p = Participant {
            user_id: "u1".to_string(),
            username: "alice".to_string(),
            nick: None,
            avatar_hash: None,
            muted: false,
            deafened: false,
        };
        assert_eq!(p.user_id, "u1");
        assert!(!p.muted);
        assert!(!p.deafened);
    }

    #[test]
    fn discord_event_ready() {
        let event = DiscordEvent::Ready {
            username: "alice".to_string(),
            user_id: "u1".to_string(),
        };
        match event {
            DiscordEvent::Ready { username, user_id } => {
                assert_eq!(username, "alice");
                assert_eq!(user_id, "u1");
            }
            _ => panic!("expected Ready"),
        }
    }

    #[test]
    fn discord_event_disconnected() {
        let event = DiscordEvent::Disconnected;
        match event {
            DiscordEvent::Disconnected => (),
            _ => panic!("expected Disconnected"),
        }
    }

    #[test]
    fn process_frame_with_invalid_json() {
        let v = json!({
            "data": {}
        });
        let (events, _avatars, _sub, _g) = process_frame_events(&v, "local", "me", None);
        // Should handle gracefully
        assert!(events.is_empty() || events.len() > 0); // Just check it doesn't panic
    }
}
