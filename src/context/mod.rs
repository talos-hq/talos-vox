//! Which application had focus when a recording started.
//!
//! The snapshot is taken at record start, not at output time, so switching
//! windows while a dictation is transcribed cannot misattribute it. It feeds
//! `VOXTYPE_APP_ID` / `VOXTYPE_APP_TITLE` for the post-process command, and
//! is the input that Modes (per-app profiles) will resolve against.
//!
//! Probes, picked from the session's environment by [`detect`]:
//! - Hyprland (`HYPRLAND_INSTANCE_SIGNATURE`): `hyprctl activewindow -j`.
//! - Sway (`SWAYSOCK`): `swaymsg -t get_tree`, the focused node.
//! - X11 sessions (`XDG_SESSION_TYPE=x11`, or `DISPLAY` without
//!   `WAYLAND_DISPLAY`): `xprop` on `_NET_ACTIVE_WINDOW`.
//! - macOS: the frontmost app's bundle identifier (`lsappinfo`). No title.
//!
//! Everywhere else (GNOME and KDE on Wayland, which expose no focus
//! protocol to clients) there is no snapshot: the variables are unset and
//! behaviour is exactly what it was before. `voxtype context` shows what
//! this session reports.

mod hyprland;
mod macos;
mod sway;
mod x11;

use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::Value;

/// The focused application.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AppContext {
    /// Stable application identifier: Wayland app_id or X11 class
    /// (e.g. `firefox`, `code`, `org.wezfurlong.wezterm`).
    pub app_id: String,
    /// Window title, when the compositor reports one.
    pub title: Option<String>,
    /// Which probe produced this (`hyprland`, `sway`).
    pub source: &'static str,
}

/// A probe gives up after this long, so a wedged compositor IPC can never
/// hold up a recording's post-processing.
const PROBE_TIMEOUT: Duration = Duration::from_millis(500);

/// Run a probe command and return its stdout, or `None` on failure/timeout.
fn run(program: &str, args: &[&str]) -> Option<String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    // Drain stdout on a thread: `swaymsg -t get_tree` can exceed the pipe
    // buffer, and a child blocked on a full pipe never exits.
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut out = String::new();
        std::io::Read::read_to_string(&mut stdout, &mut out).ok()?;
        Some(out)
    });
    let deadline = Instant::now() + PROBE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(_)) | Err(_) => return None,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                tracing::debug!("{program} did not answer within {PROBE_TIMEOUT:?}");
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
        }
    }
    reader.join().ok()?
}

fn non_empty(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// A way to ask "which application has focus" on one kind of session.
pub trait FocusProbe: Send + Sync {
    /// Short name, shown by `voxtype context`.
    fn name(&self) -> &'static str;
    /// The focused application now, or `None` if nothing is focused or the
    /// probe failed.
    fn snapshot(&self) -> Option<AppContext>;
}

/// Session facts that pick a probe, separated from the real environment so
/// the choice is testable.
#[derive(Debug, Default, Clone)]
pub struct Session {
    pub hyprland: bool,
    pub sway: bool,
    pub wayland: bool,
    pub x11_display: bool,
    pub session_type: Option<String>,
}

impl Session {
    pub fn from_env() -> Self {
        let set = |k: &str| std::env::var_os(k).is_some_and(|v| !v.is_empty());
        Self {
            hyprland: set("HYPRLAND_INSTANCE_SIGNATURE"),
            sway: set("SWAYSOCK"),
            wayland: set("WAYLAND_DISPLAY"),
            x11_display: set("DISPLAY"),
            session_type: std::env::var("XDG_SESSION_TYPE").ok(),
        }
    }
}

/// Which probe this session supports, if any.
pub fn probe_for(session: &Session) -> Option<Box<dyn FocusProbe>> {
    if session.hyprland {
        return Some(Box::new(hyprland::Hyprland));
    }
    if session.sway {
        return Some(Box::new(sway::Sway));
    }
    let x11 = session.session_type.as_deref() == Some("x11")
        || (session.x11_display && !session.wayland && session.session_type.is_none());
    if x11 {
        return Some(Box::new(x11::X11));
    }
    None
}

/// The probe for the current session, if any.
pub fn detect() -> Option<Box<dyn FocusProbe>> {
    if cfg!(target_os = "macos") {
        return Some(Box::new(macos::MacOs));
    }
    probe_for(&Session::from_env())
}

/// Probe the focused application now. `None` when no probe applies to
/// this session or the probe failed.
pub fn snapshot() -> Option<AppContext> {
    detect()?.snapshot()
}

/// The snapshot taken at the latest record start.
static CAPTURED: Mutex<Option<AppContext>> = Mutex::new(None);

/// Daemon hook, called where every recording starts. Clears the previous
/// snapshot at once, then probes on a blocking thread so the recording
/// never waits on compositor IPC.
pub fn capture_at_record_start() {
    set_captured(None);
    let Some(probe) = detect() else {
        return;
    };
    tokio::task::spawn_blocking(move || {
        let ctx = probe.snapshot();
        tracing::debug!("Focused app at record start: {ctx:?}");
        set_captured(ctx);
    });
}

fn set_captured(ctx: Option<AppContext>) {
    *CAPTURED.lock().unwrap_or_else(|e| e.into_inner()) = ctx;
}

/// The focused application when the current recording started, if known.
pub fn captured() -> Option<AppContext> {
    CAPTURED.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pick(session: Session) -> Option<&'static str> {
        probe_for(&session).map(|p| p.name())
    }

    #[test]
    fn probe_selection() {
        assert_eq!(pick(Session::default()), None);
        let hypr = Session {
            hyprland: true,
            wayland: true,
            x11_display: true,
            ..Session::default()
        };
        assert_eq!(pick(hypr), Some("hyprland"));
        let sway = Session {
            sway: true,
            wayland: true,
            ..Session::default()
        };
        assert_eq!(pick(sway), Some("sway"));
        let x11 = Session {
            x11_display: true,
            session_type: Some("x11".into()),
            ..Session::default()
        };
        assert_eq!(pick(x11), Some("x11"));
        // KDE/GNOME on Wayland: Xwayland's DISPLAY must not select xprop.
        let kde = Session {
            wayland: true,
            x11_display: true,
            session_type: Some("wayland".into()),
            ..Session::default()
        };
        assert_eq!(pick(kde), None);
        // A bare X server without logind's session type.
        let bare = Session {
            x11_display: true,
            ..Session::default()
        };
        assert_eq!(pick(bare), Some("x11"));
    }

    /// Runs on the macOS CI runner: the probe is picked and answers
    /// without panicking (a headless runner may have no frontmost app).
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_probe_runs() {
        let probe = detect().expect("macOS always has a probe");
        assert_eq!(probe.name(), "macos");
        if let Some(app) = probe.snapshot() {
            assert!(!app.app_id.is_empty());
            assert_eq!(app.source, "macos");
        }
    }

    #[test]
    fn probe_failure_is_none() {
        assert_eq!(run("voxtype-definitely-not-a-command", &[]), None);
    }
}
