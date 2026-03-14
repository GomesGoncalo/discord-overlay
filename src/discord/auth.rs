//! OAuth2 authentication with Discord.

use serde_json::json;
use std::os::unix::net::UnixStream;
use std::time::Duration;
use tracing::{error, info, warn};

use super::ipc::{load_token, read_frame, save_token, send_cmd, token_path, OP_FRAME};
use super::types::Config;

fn http_exchange(params: &str) -> Option<(String, String)> {
    let resp = ureq::post("https://discord.com/api/oauth2/token")
        .set("Content-Type", "application/x-www-form-urlencoded")
        .send_string(params)
        .ok()?;
    let v: serde_json::Value = resp.into_json().ok()?;
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

/// Distinguishes a token-rejection error from other auth failures.
#[derive(Debug)]
pub enum AuthError {
    /// Discord rejected the token (evt=ERROR, e.g. code 4009 "Invalid token").
    InvalidToken,
    /// Any other failure (I/O error, timeout, etc.).
    Other,
}

/// Send AUTHENTICATE, wait for response.
pub fn authenticate(stream: &mut UnixStream, token: &str) -> Result<String, AuthError> {
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
pub fn authorize_flow(cfg: &Config, stream: &mut UnixStream) -> Option<String> {
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

pub fn try_auth(cfg: &Config, stream: &mut UnixStream) -> Option<String> {
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

pub fn is_timeout(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::os::unix::net::UnixStream;

    #[test]
    fn authenticate_invalid_token() {
        let (mut a, mut b) = UnixStream::pair().unwrap();
        let err = json!({"nonce": "auth", "evt": "ERROR", "data": {"message": "invalid token"}});
        super::super::ipc::write_frame(&mut b, super::super::ipc::OP_FRAME, &err.to_string())
            .unwrap();
        let res = authenticate(&mut a, "TOKEN");
        assert!(matches!(res, Err(AuthError::InvalidToken)));
    }

    #[test]
    fn authenticate_success() {
        let (mut a, mut b) = UnixStream::pair().unwrap();
        let ok = json!({"nonce":"auth","cmd":"AUTHENTICATE","data":{}});
        super::super::ipc::write_frame(&mut b, super::super::ipc::OP_FRAME, &ok.to_string())
            .unwrap();
        let res = authenticate(&mut a, "TOKEN2");
        assert_eq!(res.unwrap(), "TOKEN2".to_string());
    }
}
