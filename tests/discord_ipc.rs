use serde_json::{json, Value};
use std::io;
use std::os::unix::net::{UnixListener, UnixStream};
use std::thread;
use std::time::SystemTime;

const OP_FRAME: u32 = 1;

fn write_frame(stream: &mut impl std::io::Write, op: u32, payload: &str) -> io::Result<()> {
    let data = payload.as_bytes();
    stream.write_all(&op.to_le_bytes())?;
    stream.write_all(&(data.len() as u32).to_le_bytes())?;
    stream.write_all(data)
}

fn read_frame(stream: &mut impl std::io::Read) -> io::Result<(u32, Value)> {
    let mut hdr = [0u8; 8];
    stream.read_exact(&mut hdr)?;
    let op = u32::from_le_bytes(hdr[0..4].try_into().unwrap());
    let len = u32::from_le_bytes(hdr[4..8].try_into().unwrap()) as usize;
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf)?;
    let v = serde_json::from_slice(&buf).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok((op, v))
}

#[test]
fn write_read_frame_pair() {
    let (mut a, mut b) = UnixStream::pair().expect("pair");
    let payload = json!({"cmd": "HELLO", "data": 123});
    write_frame(&mut a, OP_FRAME, &payload.to_string()).expect("write_frame");
    let (op, v) = read_frame(&mut b).expect("read_frame");
    assert_eq!(op, OP_FRAME);
    assert_eq!(v["cmd"].as_str().unwrap(), "HELLO");
    assert_eq!(v["data"].as_i64().unwrap(), 123);
}

#[test]
fn unix_listener_exchange() {
    let tmp = std::env::temp_dir();
    let uniq = SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let sock = tmp.join(format!("discord-ipc-test-{}-{}", std::process::id(), uniq));
    let _ = std::fs::remove_file(&sock);
    let listener = UnixListener::bind(&sock).expect("bind");
    let server = thread::spawn(move || {
        let (mut s, _) = listener.accept().expect("accept");
        let (op, v) = read_frame(&mut s).expect("server read_frame");
        assert_eq!(op, OP_FRAME);
        assert_eq!(v["cmd"].as_str().unwrap(), "PING");
        let resp = json!({"cmd": "PONG"});
        write_frame(&mut s, OP_FRAME, &resp.to_string()).expect("server write");
    });

    // client
    let mut client = UnixStream::connect(&sock).expect("connect");
    let msg = json!({"cmd": "PING"});
    write_frame(&mut client, OP_FRAME, &msg.to_string()).expect("client write");
    let (op2, rv) = read_frame(&mut client).expect("client read");
    assert_eq!(op2, OP_FRAME);
    assert_eq!(rv["cmd"].as_str().unwrap(), "PONG");
    server.join().unwrap();
    let _ = std::fs::remove_file(&sock);
}
