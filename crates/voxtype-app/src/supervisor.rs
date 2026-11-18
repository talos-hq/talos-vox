//! Daemon supervisor: start `voxtype daemon` when none is running, restart
//! it with backoff when it crashes, and stop it on quit, but only if this
//! app started it.
//!
//! A daemon the app did not start (a systemd unit, launchd agent, a
//! terminal) is never signalled. The supervisor only watches for it to go
//! away, and then starts its own.

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// How the supervisor waits between checks.
const TICK: Duration = Duration::from_millis(250);
/// How often a daemon owned by someone else is probed.
const EXTERNAL_PROBE: Duration = Duration::from_secs(2);
/// How long a stopping daemon gets to exit before it is killed.
const STOP_GRACE: Duration = Duration::from_secs(5);

/// What the supervisor is doing, for the window and the tray.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// `--no-supervise`: the app only talks to whatever daemon exists.
    Disabled,
    /// Not decided yet.
    Checking,
    /// A daemon this app did not start is running; it is left alone.
    External,
    /// The app started this daemon and owns it.
    Running { pid: u32, restarts: u32 },
    /// The daemon crashed (or could not be started); it is restarted when
    /// the backoff runs out.
    Backoff {
        reason: String,
        retry_in: Duration,
        restarts: u32,
    },
    /// The daemon exited cleanly on its own, or the user stopped it. Not
    /// restarted until asked.
    Stopped { reason: String },
}

impl Status {
    /// One line for the window and the tray tooltip.
    pub fn describe(&self) -> String {
        match self {
            Status::Disabled => "Supervisor off (--no-supervise)".into(),
            Status::Checking => "Looking for a running daemon...".into(),
            Status::External => "Daemon started outside voxtype-app".into(),
            Status::Running { pid, restarts: 0 } => format!("Started by voxtype-app (pid {pid})"),
            Status::Running { pid, restarts } => {
                format!("Started by voxtype-app (pid {pid}, restarted {restarts}x)")
            }
            Status::Backoff {
                reason, retry_in, ..
            } => format!(
                "Daemon {reason}; restarting in {}s",
                retry_in.as_secs_f32().ceil() as u64
            ),
            Status::Stopped { reason } => format!("Daemon {reason}"),
        }
    }

    /// Whether the app owns the daemon process (Restart applies).
    pub fn owned(&self) -> bool {
        matches!(
            self,
            Status::Running { .. } | Status::Backoff { .. } | Status::Stopped { .. }
        )
    }
}

/// Restart delays: `initial`, doubled per consecutive crash up to `max`.
/// A daemon that stayed up for `stable` resets the count.
#[derive(Debug, Clone, Copy)]
pub struct Backoff {
    pub initial: Duration,
    pub max: Duration,
    pub stable: Duration,
}

impl Default for Backoff {
    fn default() -> Self {
        Self {
            initial: Duration::from_secs(1),
            max: Duration::from_secs(60),
            stable: Duration::from_secs(30),
        }
    }
}

impl Backoff {
    /// Delay before restart number `crashes` (1-based) in a row.
    pub fn delay(&self, crashes: u32) -> Duration {
        let factor = 1u32
            .checked_shl(crashes.saturating_sub(1))
            .unwrap_or(u32::MAX);
        self.initial.saturating_mul(factor).min(self.max)
    }
}

/// The process to run as the daemon.
#[derive(Debug, Clone)]
pub struct Launch {
    pub program: PathBuf,
    pub args: Vec<String>,
    /// stdout and stderr are appended here; `None` discards them.
    pub log: Option<PathBuf>,
}

impl Launch {
    /// `voxtype daemon`, next to this executable if it is there (an
    /// installed bundle), else from `PATH`.
    pub fn daemon(program: Option<PathBuf>) -> Self {
        let program = program.unwrap_or_else(default_daemon_binary);
        Self {
            program,
            args: vec!["daemon".into()],
            log: Some(voxtype::config::Config::runtime_dir().join("voxtype-app-daemon.log")),
        }
    }

    fn spawn(&self) -> std::io::Result<Child> {
        let mut cmd = Command::new(&self.program);
        cmd.args(&self.args).stdin(Stdio::null());
        match self.log.as_deref().map(open_log) {
            Some(Ok(file)) => {
                cmd.stdout(file.try_clone()?).stderr(file);
            }
            Some(Err(e)) => {
                eprintln!("voxtype-app: daemon log not opened ({e}); discarding its output");
                cmd.stdout(Stdio::null()).stderr(Stdio::null());
            }
            None => {
                cmd.stdout(Stdio::null()).stderr(Stdio::null());
            }
        }
        platform::prepare(&mut cmd);
        cmd.spawn()
    }
}

fn open_log(path: &Path) -> std::io::Result<std::fs::File> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    OpenOptions::new().create(true).append(true).open(path)
}

fn default_daemon_binary() -> PathBuf {
    let name = if cfg!(windows) {
        "voxtype.exe"
    } else {
        "voxtype"
    };
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(name)))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from(name))
}

/// Is a daemon running that this supervisor did not start? Asks the
/// control socket, then the single-instance lockfile (a daemon too old to
/// serve the control channel still holds that).
pub fn daemon_running() -> bool {
    voxtype::control::request_v1(&voxtype::control::socket_path(), "status").is_ok()
        || voxtype::daemon_status::read_pid_if_alive().is_some()
}

enum Msg {
    Restart,
    Shutdown(Sender<()>),
}

/// Handle to the supervisor thread. Dropping it stops an owned daemon.
pub struct Supervisor {
    status: Arc<Mutex<Status>>,
    tx: Sender<Msg>,
    thread: Option<JoinHandle<()>>,
}

impl Supervisor {
    /// Start supervising. `probe` reports whether some daemon is already
    /// running; `notify` runs after every status change.
    pub fn spawn(
        launch: Launch,
        backoff: Backoff,
        probe: impl Fn() -> bool + Send + 'static,
        notify: impl Fn() + Send + 'static,
    ) -> Self {
        let status = Arc::new(Mutex::new(Status::Checking));
        let (tx, rx) = mpsc::channel();
        let shared = status.clone();
        let thread = std::thread::Builder::new()
            .name("voxtype-app-supervisor".into())
            .spawn(move || {
                Run {
                    launch,
                    backoff,
                    probe: Box::new(probe),
                    notify: Box::new(notify),
                    status: shared,
                    child: None,
                    crashes: 0,
                    restarts: 0,
                }
                .run(rx)
            })
            .expect("spawn the supervisor thread");
        Self {
            status,
            tx,
            thread: Some(thread),
        }
    }

    /// A supervisor that does nothing (`--no-supervise`).
    pub fn disabled() -> Self {
        let (tx, _rx) = mpsc::channel();
        Self {
            status: Arc::new(Mutex::new(Status::Disabled)),
            tx,
            thread: None,
        }
    }

    pub fn status(&self) -> Status {
        self.status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Stop the owned daemon (if running) and start it again now.
    pub fn restart(&self) {
        let _ = self.tx.send(Msg::Restart);
    }

    /// Stop the daemon if this app started it, and end the thread. Blocks
    /// until the daemon has exited (at most the stop grace period plus a
    /// kill).
    pub fn shutdown(&mut self) {
        let Some(thread) = self.thread.take() else {
            return;
        };
        let (done_tx, done_rx) = mpsc::channel();
        if self.tx.send(Msg::Shutdown(done_tx)).is_ok() {
            let _ = done_rx.recv_timeout(STOP_GRACE + Duration::from_secs(5));
        }
        let _ = thread.join();
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        self.shutdown();
    }
}

struct Run {
    launch: Launch,
    backoff: Backoff,
    probe: Box<dyn Fn() -> bool + Send>,
    notify: Box<dyn Fn() + Send>,
    status: Arc<Mutex<Status>>,
    /// The daemon this app started, with when it started.
    child: Option<(Child, Instant)>,
    /// Crashes in a row, for the backoff.
    crashes: u32,
    /// Restarts after crashes, for display.
    restarts: u32,
}

impl Run {
    fn set(&self, status: Status) {
        let mut cur = self.status.lock().unwrap_or_else(|e| e.into_inner());
        if *cur != status {
            *cur = status;
            drop(cur);
            (self.notify)();
        }
    }

    fn run(mut self, rx: Receiver<Msg>) {
        // When to act next: probe an external daemon, or restart ours.
        let mut next = Instant::now();
        let mut retry_at: Option<Instant> = None;
        loop {
            match rx.recv_timeout(TICK) {
                Ok(Msg::Shutdown(done)) => {
                    self.stop_child();
                    let _ = done.send(());
                    return;
                }
                Ok(Msg::Restart) => {
                    self.stop_child();
                    self.crashes = 0;
                    retry_at = None;
                    next = Instant::now();
                    self.start_if_absent(&mut retry_at);
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => {
                    self.stop_child();
                    return;
                }
                Err(RecvTimeoutError::Timeout) => {}
            }

            if let Some((child, started)) = &mut self.child {
                match child.try_wait() {
                    Ok(None) => continue,
                    Ok(Some(exit)) => {
                        let ran = started.elapsed();
                        self.child = None;
                        if exit.success() {
                            self.set(Status::Stopped {
                                reason: "exited cleanly; not restarted".into(),
                            });
                        } else {
                            if ran >= self.backoff.stable {
                                self.crashes = 0;
                            }
                            self.crashes += 1;
                            let delay = self.backoff.delay(self.crashes);
                            retry_at = Some(Instant::now() + delay);
                            self.set(Status::Backoff {
                                reason: describe_exit(exit),
                                retry_in: delay,
                                restarts: self.restarts,
                            });
                        }
                    }
                    Err(e) => eprintln!("voxtype-app: waiting on the daemon failed: {e}"),
                }
                continue;
            }

            match retry_at {
                Some(at) if Instant::now() >= at => {
                    retry_at = None;
                    self.restarts += 1;
                    self.start_if_absent(&mut retry_at);
                }
                Some(at) => {
                    // Keep the countdown in the status fresh.
                    if let Status::Backoff {
                        reason, restarts, ..
                    } = self.current()
                    {
                        let left = at.saturating_duration_since(Instant::now());
                        self.set(Status::Backoff {
                            reason,
                            retry_in: Duration::from_secs(left.as_secs_f32().ceil() as u64),
                            restarts,
                        });
                    }
                }
                None => {
                    if matches!(self.current(), Status::Stopped { .. }) {
                        continue;
                    }
                    if Instant::now() >= next {
                        next = Instant::now() + EXTERNAL_PROBE;
                        self.start_if_absent(&mut retry_at);
                    }
                }
            }
        }
    }

    fn current(&self) -> Status {
        self.status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Start the daemon unless one is already running.
    fn start_if_absent(&mut self, retry_at: &mut Option<Instant>) {
        if (self.probe)() {
            self.set(Status::External);
            return;
        }
        match self.launch.spawn() {
            Ok(child) => {
                let pid = child.id();
                self.child = Some((child, Instant::now()));
                self.set(Status::Running {
                    pid,
                    restarts: self.restarts,
                });
            }
            Err(e) => {
                self.crashes += 1;
                let delay = self.backoff.delay(self.crashes);
                *retry_at = Some(Instant::now() + delay);
                self.set(Status::Backoff {
                    reason: format!(
                        "could not be started ({}: {e})",
                        self.launch.program.display()
                    ),
                    retry_in: delay,
                    restarts: self.restarts,
                });
            }
        }
    }

    /// Ask the owned daemon to exit, then kill it after the grace period.
    fn stop_child(&mut self) {
        let Some((mut child, _)) = self.child.take() else {
            return;
        };
        let grace = match platform::terminate(&child) {
            Ok(()) => STOP_GRACE,
            Err(e) => {
                eprintln!("voxtype-app: asking the daemon to stop failed ({e}); killing it");
                Duration::ZERO
            }
        };
        let deadline = Instant::now() + grace;
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::park_timeout(Duration::from_millis(50))
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
            }
        }
        self.set(Status::Stopped {
            reason: "stopped by voxtype-app".into(),
        });
    }
}

fn describe_exit(exit: ExitStatus) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = exit.signal() {
            return format!("crashed (signal {sig})");
        }
    }
    match exit.code() {
        Some(code) => format!("crashed (exit code {code})"),
        None => "crashed".into(),
    }
}

#[cfg(unix)]
mod platform {
    use std::process::{Child, Command};

    pub fn prepare(cmd: &mut Command) {
        // If the app dies without running its shutdown (SIGKILL, a crash),
        // the kernel sends the daemon SIGTERM instead of leaving it behind.
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::process::CommandExt;
            // SAFETY: prctl is async-signal-safe and touches only the child.
            unsafe {
                cmd.pre_exec(|| {
                    libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                    Ok(())
                });
            }
        }
        #[cfg(not(target_os = "linux"))]
        let _ = cmd;
    }

    /// SIGTERM: the daemon's graceful shutdown path (state and lock files
    /// removed).
    pub fn terminate(child: &Child) -> std::io::Result<()> {
        let pid =
            libc::pid_t::try_from(child.id()).map_err(|_| std::io::ErrorKind::InvalidInput)?;
        // SAFETY: plain kill(2) on our own child's pid.
        if unsafe { libc::kill(pid, libc::SIGTERM) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
}

#[cfg(windows)]
mod platform {
    use std::process::{Child, Command};

    pub fn prepare(cmd: &mut Command) {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: no console window for the daemon.
        cmd.creation_flags(0x0800_0000);
    }

    /// kernal-api's Terminate control signal, which the daemon handles as
    /// it does SIGTERM on Unix.
    pub fn terminate(child: &Child) -> std::io::Result<()> {
        kernal_api::control_signal::send(
            child.id(),
            kernal_api::control_signal::ControlSignal::Terminate,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn fast() -> Backoff {
        Backoff {
            initial: Duration::from_millis(50),
            max: Duration::from_millis(200),
            stable: Duration::from_secs(30),
        }
    }

    /// Poll until `pred` holds or 10 s pass.
    fn wait_for(sup: &Supervisor, pred: impl Fn(&Status) -> bool) -> Status {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let s = sup.status();
            if pred(&s) || Instant::now() > deadline {
                return s;
            }
            std::thread::park_timeout(Duration::from_millis(20));
        }
    }

    #[test]
    fn backoff_doubles_to_the_cap() {
        let b = Backoff::default();
        assert_eq!(b.delay(1), Duration::from_secs(1));
        assert_eq!(b.delay(2), Duration::from_secs(2));
        assert_eq!(b.delay(4), Duration::from_secs(8));
        assert_eq!(b.delay(7), Duration::from_secs(60));
        assert_eq!(b.delay(200), Duration::from_secs(60));
    }

    #[test]
    fn descriptions() {
        assert!(Status::Running {
            pid: 7,
            restarts: 2
        }
        .describe()
        .contains("restarted 2x"));
        assert!(!Status::External.owned());
        assert!(Status::Running {
            pid: 1,
            restarts: 0
        }
        .owned());
    }

    #[test]
    fn missing_binary_backs_off() {
        let launch = Launch {
            program: PathBuf::from("/nonexistent/voxtype-app-test-daemon"),
            args: vec![],
            log: None,
        };
        let sup = Supervisor::spawn(launch, fast(), || false, || {});
        let s = wait_for(&sup, |s| matches!(s, Status::Backoff { .. }));
        match s {
            Status::Backoff { reason, .. } => assert!(reason.contains("could not be started")),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn an_external_daemon_is_left_alone() {
        let spawned = Arc::new(AtomicBool::new(false));
        let launch = Launch {
            program: PathBuf::from("/nonexistent/should-never-run"),
            args: vec![],
            log: None,
        };
        let flag = spawned.clone();
        let mut sup = Supervisor::spawn(
            launch,
            fast(),
            || true,
            move || flag.store(true, Ordering::SeqCst),
        );
        assert_eq!(wait_for(&sup, |s| *s == Status::External), Status::External);
        sup.shutdown();
        // Shutdown never reports "stopped by voxtype-app" for a daemon it
        // doesn't own.
        assert_eq!(sup.status(), Status::External);
        assert!(spawned.load(Ordering::SeqCst));
    }

    #[cfg(unix)]
    fn sh(script: &str, log: Option<PathBuf>) -> Launch {
        Launch {
            program: PathBuf::from("/bin/sh"),
            args: vec!["-c".into(), script.into()],
            log,
        }
    }

    #[cfg(unix)]
    fn alive(pid: u32) -> bool {
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }

    #[cfg(unix)]
    #[test]
    fn a_crashing_daemon_is_restarted_with_backoff() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("daemon.log");
        let sup = Supervisor::spawn(
            sh("echo started; exit 3", Some(log.clone())),
            fast(),
            || false,
            || {},
        );
        let s = wait_for(
            &sup,
            |s| matches!(s, Status::Backoff { restarts, .. } if *restarts >= 2),
        );
        match s {
            Status::Backoff { reason, .. } => assert_eq!(reason, "crashed (exit code 3)"),
            other => panic!("unexpected {other:?}"),
        }
        let started = std::fs::read_to_string(log).unwrap();
        assert!(started.lines().filter(|l| *l == "started").count() >= 3);
    }

    #[cfg(unix)]
    #[test]
    fn a_clean_exit_is_not_restarted() {
        let sup = Supervisor::spawn(sh("exit 0", None), fast(), || false, || {});
        let s = wait_for(&sup, |s| matches!(s, Status::Stopped { .. }));
        assert!(matches!(s, Status::Stopped { .. }), "{s:?}");
        std::thread::park_timeout(Duration::from_millis(300));
        assert!(matches!(sup.status(), Status::Stopped { .. }));
    }

    #[cfg(unix)]
    #[test]
    fn shutdown_stops_an_owned_daemon_gracefully() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("terminated");
        let ready = dir.path().join("ready");
        let script = format!(
            "trap 'echo term > {}; exit 0' TERM; touch {}; while :; do sleep 0.05; done",
            marker.display(),
            ready.display()
        );
        let mut sup = Supervisor::spawn(sh(&script, None), fast(), || false, || {});
        let pid = match wait_for(&sup, |s| matches!(s, Status::Running { .. })) {
            Status::Running { pid, .. } => pid,
            other => panic!("unexpected {other:?}"),
        };
        // Signal only once the trap is installed.
        let deadline = Instant::now() + Duration::from_secs(10);
        while !ready.exists() && Instant::now() < deadline {
            std::thread::park_timeout(Duration::from_millis(20));
        }
        assert!(alive(pid));
        sup.shutdown();
        assert!(!alive(pid), "daemon {pid} survived shutdown");
        assert!(marker.exists(), "the daemon got SIGTERM, not SIGKILL");
        assert!(matches!(sup.status(), Status::Stopped { .. }));
    }

    #[cfg(unix)]
    #[test]
    fn restart_replaces_the_process() {
        let mut sup = Supervisor::spawn(sh("exec sleep 60", None), fast(), || false, || {});
        let first = match wait_for(&sup, |s| matches!(s, Status::Running { .. })) {
            Status::Running { pid, .. } => pid,
            other => panic!("unexpected {other:?}"),
        };
        sup.restart();
        let second = match wait_for(
            &sup,
            |s| matches!(s, Status::Running { pid, .. } if *pid != first),
        ) {
            Status::Running { pid, .. } => pid,
            other => panic!("unexpected {other:?}"),
        };
        assert!(!alive(first));
        sup.shutdown();
        assert!(!alive(second));
    }

    #[cfg(windows)]
    #[test]
    fn a_crashing_daemon_is_restarted_with_backoff() {
        let launch = Launch {
            program: PathBuf::from("cmd.exe"),
            args: vec!["/C".into(), "exit 3".into()],
            log: None,
        };
        let sup = Supervisor::spawn(launch, fast(), || false, || {});
        let s = wait_for(
            &sup,
            |s| matches!(s, Status::Backoff { restarts, .. } if *restarts >= 2),
        );
        match s {
            Status::Backoff { reason, .. } => assert_eq!(reason, "crashed (exit code 3)"),
            other => panic!("unexpected {other:?}"),
        }
    }
}
