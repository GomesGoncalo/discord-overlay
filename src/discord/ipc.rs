//! Low-level IPC protocol: socket finding, frame I/O, token caching.

use serde_json::{json, Value};
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use tracing::{info, warn};

/// Frame structure: [op: u32 LE][len: u32 LE][payload: UTF-8 JSON]
pub const OP_HANDSHAKE: u32 = 0;
pub const OP_FRAME: u32 = 1;

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

pub fn find_socket() -> Option<UnixStream> {
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

pub fn write_frame(stream: &mut impl Write, op: u32, payload: &str) -> io::Result<()> {
    let data = payload.as_bytes();
    stream.write_all(&op.to_le_bytes())?;
    stream.write_all(&(data.len() as u32).to_le_bytes())?;
    stream.write_all(data)
}

/// Maximum IPC frame payload size (16 MiB). Discord frames are tiny in practice;
/// this guards against corrupt or malicious length fields causing huge allocations.
const MAX_FRAME_LEN: usize = 16 * 1024 * 1024;

pub fn read_frame(stream: &mut impl Read) -> io::Result<(u32, Value)> {
    let mut hdr = [0u8; 8];
    stream.read_exact(&mut hdr)?;
    let op = u32::from_le_bytes(
        hdr[0..4]
            .try_into()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid opcode header"))?,
    );
    let len = u32::from_le_bytes(
        hdr[4..8]
            .try_into()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid length header"))?,
    ) as usize;
    if len > MAX_FRAME_LEN {
        warn!(
            len,
            max = MAX_FRAME_LEN,
            "IPC frame rejected: payload exceeds size limit"
        );
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("frame too large: {len} bytes (max {MAX_FRAME_LEN})"),
        ));
    }
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf)?;
    let v =
        serde_json::from_slice(&buf).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok((op, v))
}

pub fn is_timeout(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

pub fn send_cmd(stream: &mut UnixStream, msg: Value) {
    let _ = write_frame(stream, OP_FRAME, &msg.to_string());
}

pub fn token_path() -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    let dir = std::path::Path::new(&home)
        .join(".cache")
        .join("hypr-overlay");
    let _ = std::fs::create_dir_all(&dir);
    dir.join("discord-token.json")
}

pub fn load_token() -> Option<(String, String)> {
    let data = std::fs::read_to_string(token_path()).ok()?;
    let v: Value = serde_json::from_str(&data).ok()?;
    Some((
        v["access_token"].as_str()?.to_string(),
        v["refresh_token"].as_str()?.to_string(),
    ))
}

pub fn save_token(access: &str, refresh: &str) {
    let _ = std::fs::write(
        token_path(),
        json!({"access_token": access, "refresh_token": refresh}).to_string(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn frame_roundtrip() {
        let op = 1u32;
        let payload = r#"{"hello":"world"}"#;
        let mut buf = Vec::new();
        write_frame(&mut buf, op, payload).expect("write");

        let mut c = Cursor::new(buf);
        let (got_op, got_val) = read_frame(&mut c).expect("read");
        assert_eq!(got_op, op);
        assert_eq!(got_val["hello"].as_str().unwrap(), "world");
    }

    #[test]
    fn write_frame_basic() {
        let mut buf = Vec::new();
        let payload = "test";
        write_frame(&mut buf, 1, payload).expect("write");
        assert!(!buf.is_empty());
        // Frame: op(4) + len(4) + payload
        assert!(buf.len() >= 8);
    }
}
