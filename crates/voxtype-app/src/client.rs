//! The app's side of the daemon boundary: control protocol v1 requests
//! (`status`, `start`, `stop`) and the opt-in history database, read on a
//! worker thread so the UI never blocks on a socket or SQLite.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use serde_json::Value;
use voxtype::history::Entry;

/// How often the worker asks the daemon for its state. The control
/// protocol has no subscription, and a status request is one local
/// round trip.
pub const STATUS_INTERVAL: Duration = Duration::from_millis(500);
/// How often the history list is re-read while nothing else triggers it.
pub const HISTORY_INTERVAL: Duration = Duration::from_secs(5);
/// Rows shown in the history list.
pub const HISTORY_LIMIT: usize = 50;

/// A request the UI sends to the worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    Start,
    Stop,
    RefreshHistory,
}

/// The daemon's state as last seen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonState {
    /// Not asked yet.
    Unknown,
    /// The state word from the daemon (`idle`, `recording`, `transcribing`,
    /// ...).
    Running(String),
    /// No daemon answered on the control socket.
    Unreachable(String),
}

/// What the history pane shows.
#[derive(Debug, Clone, PartialEq)]
pub enum History {
    /// `[history] enabled = false`, the default: nothing is stored.
    Disabled,
    Entries(Vec<Entry>),
    Error(String),
}

/// Everything the UI renders, sent by the worker after each change.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub daemon: DaemonState,
    pub history: History,
    /// The last start/stop failure, cleared by the next success.
    pub last_error: Option<String>,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            daemon: DaemonState::Unknown,
            history: History::Entries(Vec::new()),
            last_error: None,
        }
    }
}

/// The daemon side, abstracted so the UI and worker are testable without
/// a running daemon.
pub trait Daemon: Send + 'static {
    /// Send one v1 command and return the reported state.
    fn command(&self, cmd: &str) -> Result<String, String>;
    /// Newest first, at most `limit`.
    fn history(&self, limit: usize) -> History;
}

/// The real daemon: the control socket and the history database.
pub struct Local {
    pub socket: PathBuf,
    pub history_enabled: bool,
    pub history_db: PathBuf,
}

impl Local {
    /// Paths and settings from the user's config, as the CLI resolves them.
    pub fn from_config() -> Self {
        let config = voxtype::config::load_config(None).unwrap_or_else(|e| {
            eprintln!("voxtype-app: config not loaded ({e}); using defaults");
            voxtype::config::Config::default()
        });
        Self {
            socket: voxtype::control::socket_path(),
            history_enabled: config.history.enabled,
            history_db: voxtype::history::default_path(),
        }
    }
}

impl Daemon for Local {
    fn command(&self, cmd: &str) -> Result<String, String> {
        let reply = voxtype::control::request_v1(&self.socket, cmd)
            .map_err(|e| format!("no daemon on {}: {e}", self.socket.display()))?;
        parse_reply(&reply)
    }

    fn history(&self, limit: usize) -> History {
        if !self.history_enabled {
            return History::Disabled;
        }
        if !self.history_db.exists() {
            return History::Entries(Vec::new());
        }
        match voxtype::history::Store::open(&self.history_db).and_then(|s| s.list(limit)) {
            Ok(entries) => History::Entries(entries),
            Err(e) => History::Error(format!("{}: {e}", self.history_db.display())),
        }
    }
}

/// Read a v1 reply: `{"v":1,"ok":true,"state":"idle"}` or
/// `{"v":1,"ok":false,"error":"..."}`.
pub fn parse_reply(reply: &Value) -> Result<String, String> {
    if reply.get("v").and_then(Value::as_u64) != Some(voxtype::control::PROTOCOL_VERSION) {
        return Err(format!("unexpected reply: {reply}"));
    }
    if reply.get("ok").and_then(Value::as_bool) == Some(true) {
        Ok(reply
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string())
    } else {
        Err(reply
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("request failed")
            .to_string())
    }
}

/// Handle to the worker thread.
pub struct Worker {
    pub requests: Sender<Request>,
    pub snapshots: Receiver<Snapshot>,
}

impl Worker {
    /// Start the worker. `notify` runs after every new snapshot (the UI
    /// passes `request_repaint`).
    pub fn spawn(daemon: impl Daemon, notify: impl Fn() + Send + 'static) -> Self {
        let (requests, rx) = mpsc::channel();
        let (tx, snapshots) = mpsc::channel();
        std::thread::Builder::new()
            .name("voxtype-app-daemon".into())
            .spawn(move || run(daemon, rx, tx, notify))
            .expect("spawn the daemon worker thread");
        Self {
            requests,
            snapshots,
        }
    }
}

/// The worker loop: wait for a request or the next status tick, talk to
/// the daemon, publish a snapshot when anything changed. Exits when the UI
/// drops its end.
fn run(
    daemon: impl Daemon,
    requests: Receiver<Request>,
    snapshots: Sender<Snapshot>,
    notify: impl Fn(),
) {
    let mut snap = Snapshot::default();
    let mut published: Option<Snapshot> = None;
    let mut next_history = Instant::now();
    loop {
        let mut refresh_history = false;
        match requests.recv_timeout(STATUS_INTERVAL) {
            Ok(req) => match req {
                Request::Start | Request::Stop => {
                    let cmd = if req == Request::Start {
                        "start"
                    } else {
                        "stop"
                    };
                    match daemon.command(cmd) {
                        Ok(_) => snap.last_error = None,
                        Err(e) => snap.last_error = Some(e),
                    }
                }
                Request::RefreshHistory => refresh_history = true,
            },
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        let previous = snap.daemon.clone();
        snap.daemon = match daemon.command("status") {
            Ok(state) => DaemonState::Running(state),
            Err(e) => DaemonState::Unreachable(e),
        };
        // A dictation lands in history when the daemon goes back to idle.
        let finished = matches!(&previous, DaemonState::Running(s) if s != "idle")
            && snap.daemon == DaemonState::Running("idle".into());
        if refresh_history || finished || Instant::now() >= next_history {
            snap.history = daemon.history(HISTORY_LIMIT);
            next_history = Instant::now() + HISTORY_INTERVAL;
        }
        if published.as_ref() != Some(&snap) {
            if snapshots.send(snap.clone()).is_err() {
                return;
            }
            published = Some(snap.clone());
            notify();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    #[test]
    fn replies() {
        assert_eq!(
            parse_reply(&json!({"v":1,"ok":true,"state":"recording"})),
            Ok("recording".into())
        );
        assert_eq!(
            parse_reply(&json!({"v":1,"ok":false,"error":"unknown request: fly"})),
            Err("unknown request: fly".into())
        );
        // A legacy reply (no "v") or another version is not ours.
        assert!(parse_reply(&json!({"ok":true,"state":"idle"})).is_err());
        assert!(parse_reply(&json!({"v":2,"ok":true,"state":"idle"})).is_err());
    }

    /// A daemon that records commands and walks idle -> recording ->
    /// transcribing -> idle.
    #[derive(Clone, Default)]
    pub(crate) struct Fake {
        pub log: Arc<Mutex<Vec<String>>>,
        pub state: Arc<Mutex<String>>,
        pub history_reads: Arc<Mutex<usize>>,
    }

    impl Daemon for Fake {
        fn command(&self, cmd: &str) -> Result<String, String> {
            self.log.lock().unwrap().push(cmd.to_string());
            let mut state = self.state.lock().unwrap();
            match cmd {
                "start" => *state = "recording".into(),
                "stop" => *state = "idle".into(),
                _ => {}
            }
            if state.is_empty() {
                *state = "idle".into();
            }
            Ok(state.clone())
        }
        fn history(&self, _limit: usize) -> History {
            *self.history_reads.lock().unwrap() += 1;
            History::Entries(Vec::new())
        }
    }

    #[test]
    fn worker_forwards_commands_and_reports_state() {
        let fake = Fake::default();
        let worker = Worker::spawn(fake.clone(), || {});
        let first = worker
            .snapshots
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        assert_eq!(first.daemon, DaemonState::Running("idle".into()));
        worker.requests.send(Request::Start).unwrap();
        let snap = worker
            .snapshots
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        assert_eq!(snap.daemon, DaemonState::Running("recording".into()));
        let reads_before = *fake.history_reads.lock().unwrap();
        worker.requests.send(Request::Stop).unwrap();
        let snap = worker
            .snapshots
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        assert_eq!(snap.daemon, DaemonState::Running("idle".into()));
        // recording -> idle re-reads history.
        assert!(*fake.history_reads.lock().unwrap() > reads_before);
        let log = fake.log.lock().unwrap().clone();
        assert!(log.contains(&"start".to_string()) && log.contains(&"stop".to_string()));
    }

    /// The real client against the daemon's real control server.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn talks_to_the_control_server() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("control.sock");
        let state = Arc::new(Mutex::new("idle".to_string()));
        let mut commands = voxtype::control::serve(&socket, state).unwrap();
        let local = Local {
            socket,
            history_enabled: false,
            history_db: dir.path().join("history.db"),
        };
        let (status, start, stop) = tokio::task::spawn_blocking(move || {
            (
                local.command("status"),
                local.command("start"),
                local.command("stop"),
            )
        })
        .await
        .unwrap();
        assert_eq!(status, Ok("idle".into()));
        assert!(start.is_ok() && stop.is_ok());
        assert_eq!(
            commands.recv().await,
            Some(voxtype::control::Command::Start)
        );
        assert_eq!(commands.recv().await, Some(voxtype::control::Command::Stop));
    }

    #[test]
    fn disabled_history_is_reported() {
        let local = Local {
            socket: PathBuf::from("/nonexistent/voxtype-app-test.sock"),
            history_enabled: false,
            history_db: PathBuf::from("/nonexistent/history.db"),
        };
        assert_eq!(local.history(10), History::Disabled);
        assert!(local.command("status").is_err());
    }

    #[test]
    fn enabled_history_reads_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("history.db");
        let store = voxtype::history::Store::open(&db).unwrap();
        store
            .insert(1_000, "hello world", "whisper", "base.en")
            .unwrap();
        drop(store);
        let local = Local {
            socket: dir.path().join("none.sock"),
            history_enabled: true,
            history_db: db,
        };
        match local.history(10) {
            History::Entries(e) => {
                assert_eq!(e.len(), 1);
                assert_eq!(e[0].text, "hello world");
            }
            other => panic!("unexpected {other:?}"),
        }
    }
}
