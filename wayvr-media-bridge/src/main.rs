//! Firefox native-messaging host bridging the WayVR YT Music extension to a
//! running wayvr instance over wayvr's existing IPC socket.
//!
//! Two different framings meet here and this process reshapes between them:
//!
//!   Native messaging (stdin/stdout): u32 *native-endian* length + UTF-8 JSON,
//!     where JSON is the extension's `{type:"state"|"cmd", ...}` shape.
//!   wayvr IPC (`/tmp/wayvr_ipc.sock`, abstract namespace): u32 *big-endian*
//!     length + serde_json of a `PacketClient`/`PacketServer`, after a handshake.
//!
//!   stdin  {type:"state",...}                 -> PacketClient::WatchMediaState
//!   PacketServer::WatchMediaCommand(...)       -> stdout {type:"cmd",action:...}
//!
//! All logging goes to stderr; stdout carries the native-messaging stream. If
//! either side closes, the process exits so the extension respawns it.

use std::io::{self, BufReader, Read, Write};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use interprocess::local_socket::{GenericNamespaced, Stream, ToNsName, traits::Stream as _};
use serde::Deserialize;
use wayvr_ipc::{
    ipc::{self, CONNECTION_MAGIC, MEDIA_BRIDGE_CLIENT_NAME, PROTOCOL_VERSION},
    packet_client::{Handshake, PacketClient, WatchMediaState},
    packet_server::{PacketServer, WatchMediaCommand},
};

const SOCKET_NAME: &str = "/tmp/wayvr_ipc.sock";
const CLIENT_NAME: &str = MEDIA_BRIDGE_CLIENT_NAME;

/// The JSON the browser extension sends; mirrors `WatchMediaState` plus an
/// ignored `type` tag.
#[derive(Deserialize)]
struct ExtState {
    #[serde(rename = "type")]
    _msg_type: String,
    mediastate: WatchMediaState,
}

/// wayvr may not be up yet when Firefox spawns us; retry briefly.
fn connect_with_retry() -> io::Result<Stream> {
    let mut last_err = None;
    for _ in 0..20 {
        let name = SOCKET_NAME.to_ns_name::<GenericNamespaced>()?;
        match Stream::connect(name) {
            Ok(stream) => return Ok(stream),
            Err(err) => {
                last_err = Some(err);
                thread::sleep(Duration::from_millis(250));
            }
        }
    }
    Err(last_err.unwrap_or_else(|| io::Error::other("no wayvr IPC socket")))
}

// --- wayvr IPC framing (u32 big-endian length + payload) ---

fn send_ipc<W: Write>(writer: &mut W, payload: &[u8]) -> io::Result<()> {
    writer.write_all(&(payload.len() as u32).to_be_bytes())?;
    writer.write_all(payload)?;
    writer.flush()
}

fn read_ipc<R: Read>(reader: &mut R) -> io::Result<Option<Vec<u8>>> {
    let mut len_buf = [0u8; 4];
    match reader.read_exact(&mut len_buf) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(err) => return Err(err),
    }
    let len = u32::from_be_bytes(len_buf) as usize;
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf)?;
    Ok(Some(buf))
}

// --- native-messaging framing (u32 native-endian length + payload) ---

fn read_message<R: Read>(reader: &mut R) -> io::Result<Option<Vec<u8>>> {
    let mut len_buf = [0u8; 4];
    match reader.read_exact(&mut len_buf) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(err) => return Err(err),
    }
    let len = u32::from_ne_bytes(len_buf) as usize;
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf)?;
    Ok(Some(buf))
}

fn write_message<W: Write>(writer: &mut W, payload: &[u8]) -> io::Result<()> {
    writer.write_all(&(payload.len() as u32).to_ne_bytes())?;
    writer.write_all(payload)?;
    writer.flush()
}

fn main() {
    let stream = match connect_with_retry() {
        Ok(stream) => Arc::new(stream),
        Err(err) => {
            eprintln!("wayvr-media-bridge: cannot reach wayvr IPC socket: {err}");
            std::process::exit(1);
        }
    };

    // `&Stream` implements both Read and Write, so the two threads can share one
    // connection: this thread writes, the reader thread reads.
    let mut writer: &Stream = &stream;

    let handshake = PacketClient::Handshake(Handshake {
        protocol_version: PROTOCOL_VERSION,
        magic: CONNECTION_MAGIC.to_string(),
        client_name: CLIENT_NAME.to_string(),
    });
    if let Err(err) = send_ipc(&mut writer, &ipc::data_encode(&handshake)) {
        eprintln!("wayvr-media-bridge: handshake failed: {err}");
        std::process::exit(1);
    }

    // wayvr IPC -> stdout: relay media commands to Firefox, ignore everything
    // else wayvr broadcasts (handshake ack, state-changed, etc.).
    let reader_stream = Arc::clone(&stream);
    thread::spawn(move || {
        let mut reader = BufReader::new(&*reader_stream);
        let stdout = io::stdout();
        loop {
            match read_ipc(&mut reader) {
                Ok(Some(payload)) => match ipc::data_decode::<PacketServer>(&payload) {
                    Ok(PacketServer::WatchMediaCommand(command)) => {
                        let action = match command {
                            WatchMediaCommand::PlayPause => "play_pause",
                            WatchMediaCommand::Next => "next",
                        };
                        let json = format!("{{\"type\":\"cmd\",\"action\":\"{action}\"}}");
                        if write_message(&mut stdout.lock(), json.as_bytes()).is_err() {
                            break;
                        }
                    }
                    Ok(_) => {}
                    Err(err) => eprintln!("wayvr-media-bridge: decode error: {err}"),
                },
                Ok(None) | Err(_) => break,
            }
        }
        std::process::exit(0);
    });

    // stdin -> wayvr IPC: forward playback state from Firefox.
    let stdin = io::stdin();
    let mut stdin = stdin.lock();
    loop {
        match read_message(&mut stdin) {
            Ok(Some(payload)) => {
                let ext = match serde_json::from_slice::<ExtState>(&payload) {
                    Ok(ext) => ext,
                    Err(err) => {
                        eprintln!("wayvr-media-bridge: invalid message from extension: {err}");
                        let json_str = String::from_utf8_lossy(&payload);
                        eprintln!("  payload was: {json_str}");
                        continue;
                    }
                };
                let packet = PacketClient::WatchMediaState(ext.mediastate);
                if send_ipc(&mut writer, &ipc::data_encode(&packet)).is_err() {
                    break;
                }
            }
            Ok(None) | Err(_) => break, // Firefox closed stdin
        }
    }
}
