//! Daemon control channel: `record start/stop` and status over local IPC.
//!
//! `record start/stop` signals the daemon (SIGUSR1/SIGUSR2 on Unix,
//! kernal-api control signals elsewhere); this channel answers the same
//! requests plus `status` on every host, for clients that want a reply,
//! and is the route on non-Unix builds without kernal-api. The protocol is one request line
//! (`start`, `stop` or `status`) and one JSON reply line:
//! `{"ok": true, "state": "<daemon state>"}`, or
//! `{"ok": false, "error": "<reason>"}` for an unknown request.

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

fn reply_for(line: &str, state: &SharedState, commands: &mpsc::UnboundedSender<Command>) -> String {
    let command = match line.trim() {
        "start" => Some(Command::Start),
        "stop" => Some(Command::Stop),
        "status" => None,
        other => {
            return serde_json::json!({ "ok": false, "error": format!("unknown request: {other}") })
                .to_string();
        }
    };
    if let Some(command) = command {
        if commands.send(command).is_err() {
            return serde_json::json!({ "ok": false, "error": "daemon is shutting down" })
                .to_string();
        }
    }
    let state = state.lock().map(|s| s.clone()).unwrap_or_default();
    serde_json::json!({ "ok": true, "state": state }).to_string()
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

/// Send one request (`start`, `stop` or `status`) and return the JSON reply.
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

    #[tokio::test]
    async fn requests_round_trip_and_forward_commands() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("control.sock");
        let state: SharedState = Arc::new(Mutex::new("idle".to_string()));
        let mut commands = serve(&path, state.clone()).unwrap();

        let client_path = path.clone();
        let replies = tokio::task::spawn_blocking(move || {
            ["status", "start", "stop", "bogus"].map(|r| request(&client_path, r).unwrap())
        })
        .await
        .unwrap();

        for reply in &replies[..3] {
            assert_eq!(reply["ok"], true);
            assert_eq!(reply["state"], "idle");
        }
        assert_eq!(replies[3]["ok"], false);
        assert_eq!(commands.recv().await, Some(Command::Start));
        assert_eq!(commands.recv().await, Some(Command::Stop));
    }
}
