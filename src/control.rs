//! Daemon control channel: `record start/stop` and status over local IPC.
//!
//! `record start/stop` signals the daemon (SIGUSR1/SIGUSR2 on Unix,
//! kernal-api control signals elsewhere); this channel answers the same
//! requests plus `status` on every host, for clients that want a reply,
//! and is the route on non-Unix builds without kernal-api.
//!
//! Each request is one line and gets one JSON reply line. Two dialects:
//!
//! - **Legacy** (accepted forever, replies locked byte-for-byte by tests):
//!   a bare word `start`, `stop` or `status`, answered with
//!   `{"ok":true,"state":"<daemon state>"}`, or
//!   `{"error":"<reason>","ok":false}` for an unknown word.
//! - **v1**: a JSON object `{"v":1,"cmd":"<command>"}` with an optional
//!   `"id"` (any JSON value) that is echoed back. Replies always carry
//!   `"v":1` and `"ok"`; success adds `"state"`, failure adds `"error"`.
//!   v1 commands today: `start`, `stop`, `status`. A request with another
//!   `"v"` is refused with `ok:false`, so clients can probe the version.

use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

use crate::local_ipc;

/// A request that drives the daemon's recording state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Start,
    Stop,
}

/// Where the daemon listens for control requests.
pub fn socket_path() -> PathBuf {
    crate::config::Config::runtime_dir().join("control.sock")
}

/// The state string the daemon publishes, shared with the control server.
pub type SharedState = Arc<Mutex<String>>;

/// Protocol version spoken by the JSON dialect.
pub const PROTOCOL_VERSION: u64 = 1;

/// What a request asks for, independent of dialect.
enum Request {
    Command(Option<Command>),
    Invalid(String),
}

fn parse_command(word: &str) -> Request {
    match word {
        "start" => Request::Command(Some(Command::Start)),
        "stop" => Request::Command(Some(Command::Stop)),
        "status" => Request::Command(None),
        other => Request::Invalid(format!("unknown request: {other}")),
    }
}

/// Run `request` against the daemon and return `(ok, state_or_error)`.
fn execute(
    request: Request,
    state: &SharedState,
    commands: &mpsc::UnboundedSender<Command>,
) -> Result<String, String> {
    let command = match request {
        Request::Command(command) => command,
        Request::Invalid(error) => return Err(error),
    };
    if let Some(command) = command {
        if commands.send(command).is_err() {
            return Err("daemon is shutting down".to_string());
        }
    }
    Ok(state.lock().map(|s| s.clone()).unwrap_or_default())
}

fn reply_for(line: &str, state: &SharedState, commands: &mpsc::UnboundedSender<Command>) -> String {
    let line = line.trim();
    if line.starts_with('{') {
        return reply_v1(line, state, commands);
    }
    // Legacy dialect. These shapes are a contract: see the byte-for-byte
    // tests below before touching them.
    match execute(parse_command(line), state, commands) {
        Ok(state) => serde_json::json!({ "ok": true, "state": state }).to_string(),
        Err(error) => serde_json::json!({ "ok": false, "error": error }).to_string(),
    }
}

fn reply_v1(line: &str, state: &SharedState, commands: &mpsc::UnboundedSender<Command>) -> String {
    let mut reply = serde_json::Map::new();
    reply.insert("v".into(), PROTOCOL_VERSION.into());
    let outcome = match serde_json::from_str::<serde_json::Value>(line) {
        Err(e) => Err(format!("invalid JSON request: {e}")),
        Ok(request) => {
            if let Some(id) = request.get("id") {
                reply.insert("id".into(), id.clone());
            }
            match (
                request.get("v").and_then(|v| v.as_u64()),
                request.get("cmd").and_then(|c| c.as_str()),
            ) {
                (Some(PROTOCOL_VERSION), Some(cmd)) => execute(parse_command(cmd), state, commands),
                (Some(PROTOCOL_VERSION), None) => Err("request has no string \"cmd\"".to_string()),
                (Some(v), _) => Err(format!(
                    "unsupported protocol version {v}; this daemon speaks v{PROTOCOL_VERSION}"
                )),
                (None, _) => Err("request has no integer \"v\"".to_string()),
            }
        }
    };
    match outcome {
        Ok(state) => {
            reply.insert("ok".into(), true.into());
            reply.insert("state".into(), state.into());
        }
        Err(error) => {
            reply.insert("ok".into(), false.into());
            reply.insert("error".into(), error.into());
        }
    }
    serde_json::Value::Object(reply).to_string()
}

async fn serve_connection(
    stream: local_ipc::Stream,
    state: SharedState,
    commands: mpsc::UnboundedSender<Command>,
) {
    let (read, mut write) = tokio::io::split(stream);
    let mut lines = BufReader::new(read).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let mut reply = reply_for(&line, &state, &commands);
        reply.push('\n');
        if AsyncWriteExt::write_all(&mut write, reply.as_bytes())
            .await
            .is_err()
        {
            break;
        }
    }
}

/// Bind the control endpoint and serve it until the returned receiver is
/// dropped. Requests that change recording state arrive on the receiver.
pub fn serve(path: &Path, state: SharedState) -> io::Result<mpsc::UnboundedReceiver<Command>> {
    if path.exists() {
        let _ = std::fs::remove_file(path);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let listener = local_ipc::Listener::bind(path)?;
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok(stream) => {
                    tokio::spawn(serve_connection(stream, state.clone(), tx.clone()));
                }
                Err(e) => {
                    if tx.is_closed() {
                        break;
                    }
                    tracing::warn!("Control socket accept error: {}", e);
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
            if tx.is_closed() {
                break;
            }
        }
    });
    Ok(rx)
}

/// Send one v1 command (`start`, `stop` or `status`) and return the reply
/// object. A reply with `"ok": false` is returned as is, not as an error.
pub fn request_v1(path: &Path, cmd: &str) -> io::Result<serde_json::Value> {
    request(
        path,
        &serde_json::json!({ "v": PROTOCOL_VERSION, "cmd": cmd }).to_string(),
    )
}

/// Send one raw request line (a legacy word, or a v1 JSON object) and
/// return the JSON reply.
pub fn request(path: &Path, request: &str) -> io::Result<serde_json::Value> {
    let mut stream = local_ipc::connect_blocking(path)?;
    stream.write_all(format!("{request}\n").as_bytes())?;
    stream.flush()?;
    let mut line = String::new();
    io::BufReader::new(stream).read_line(&mut line)?;
    serde_json::from_str(&line).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn harness() -> (
        SharedState,
        mpsc::UnboundedSender<Command>,
        mpsc::UnboundedReceiver<Command>,
    ) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Arc::new(Mutex::new("idle".to_string())), tx, rx)
    }

    /// The legacy dialect is a contract with existing clients (`voxtype
    /// record` on non-Unix hosts, scripts): these bytes must never change.
    #[test]
    fn legacy_replies_are_byte_identical() {
        let (state, tx, mut rx) = harness();
        for (line, expected) in [
            ("status", r#"{"ok":true,"state":"idle"}"#),
            ("start", r#"{"ok":true,"state":"idle"}"#),
            ("stop", r#"{"ok":true,"state":"idle"}"#),
            ("  status \r", r#"{"ok":true,"state":"idle"}"#),
            ("bogus", r#"{"error":"unknown request: bogus","ok":false}"#),
            ("", r#"{"error":"unknown request: ","ok":false}"#),
        ] {
            assert_eq!(reply_for(line, &state, &tx), expected, "request {line:?}");
        }
        assert_eq!(rx.try_recv(), Ok(Command::Start));
        assert_eq!(rx.try_recv(), Ok(Command::Stop));
        assert!(rx.try_recv().is_err());

        drop(rx);
        assert_eq!(
            reply_for("start", &state, &tx),
            r#"{"error":"daemon is shutting down","ok":false}"#
        );
    }

    #[test]
    fn v1_commands_echo_id_and_version() {
        let (state, tx, mut rx) = harness();
        let reply = |line: &str| -> serde_json::Value {
            serde_json::from_str(&reply_for(line, &state, &tx)).unwrap()
        };
        assert_eq!(
            reply(r#"{"v":1,"cmd":"status","id":7}"#),
            serde_json::json!({"v":1,"id":7,"ok":true,"state":"idle"})
        );
        assert_eq!(
            reply(r#"{"v":1,"cmd":"start"}"#),
            serde_json::json!({"v":1,"ok":true,"state":"idle"})
        );
        assert_eq!(reply(r#"{"v":1,"cmd":"stop","id":"x"}"#)["id"], "x");
        assert_eq!(rx.try_recv(), Ok(Command::Start));
        assert_eq!(rx.try_recv(), Ok(Command::Stop));

        for (line, needle) in [
            (r#"{"v":1,"cmd":"fly"}"#, "unknown request: fly"),
            (
                r#"{"v":2,"cmd":"status"}"#,
                "unsupported protocol version 2",
            ),
            (r#"{"cmd":"status"}"#, "no integer \"v\""),
            (r#"{"v":1}"#, "no string \"cmd\""),
            (r#"{"v":1,"cmd":"#, "invalid JSON request"),
        ] {
            let r = reply(line);
            assert_eq!(r["v"], 1, "{line}");
            assert_eq!(r["ok"], false, "{line}");
            assert!(r["error"].as_str().unwrap().contains(needle), "{line}: {r}");
        }
        assert!(rx.try_recv().is_err(), "failed requests must not act");
    }

    #[tokio::test]
    async fn requests_round_trip_and_forward_commands() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("control.sock");
        let state: SharedState = Arc::new(Mutex::new("idle".to_string()));
        let mut commands = serve(&path, state.clone()).unwrap();

        let client_path = path.clone();
        let (replies, v1) = tokio::task::spawn_blocking(move || {
            let legacy =
                ["status", "start", "stop", "bogus"].map(|r| request(&client_path, r).unwrap());
            (legacy, request_v1(&client_path, "status").unwrap())
        })
        .await
        .unwrap();

        for reply in &replies[..3] {
            assert_eq!(reply["ok"], true);
            assert_eq!(reply["state"], "idle");
        }
        assert_eq!(replies[3]["ok"], false);
        assert_eq!(v1, serde_json::json!({"v":1,"ok":true,"state":"idle"}));
        assert_eq!(commands.recv().await, Some(Command::Start));
        assert_eq!(commands.recv().await, Some(Command::Stop));
    }
}
