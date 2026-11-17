//! Which application had focus when a recording started.
//!
//! The snapshot is taken at record start, not at output time, so switching
//! windows while a dictation is transcribed cannot misattribute it. It feeds
//! `VOXTYPE_APP_ID` / `VOXTYPE_APP_TITLE` for the post-process command, and
//! is the input that Modes (per-app profiles) will resolve against.
//!
//! Probes, picked from the session's environment:
//! - Hyprland (`HYPRLAND_INSTANCE_SIGNATURE`): `hyprctl activewindow -j`.
//! - Sway (`SWAYSOCK`): `swaymsg -t get_tree`, the focused node.
//!
//! Everywhere else (GNOME, KDE, X11 without a probe yet, macOS and Windows
//! until their probes land) there is no snapshot: the variables are unset
//! and behaviour is exactly what it was before.

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

/// Parse `hyprctl activewindow -j`. An empty workspace answers `{}`.
pub fn parse_hyprland(json: &str) -> Option<AppContext> {
    let v: Value = serde_json::from_str(json).ok()?;
    let app_id = non_empty(v.get("class")).or_else(|| non_empty(v.get("initialClass")))?;
    Some(AppContext {
        app_id,
        title: non_empty(v.get("title")),
        source: "hyprland",
    })
}

/// Parse `swaymsg -t get_tree`: the focused leaf's `app_id` (Wayland) or
/// `window_properties.class` (Xwayland).
pub fn parse_sway(json: &str) -> Option<AppContext> {
    fn focused(node: &Value) -> Option<&Value> {
        if node.get("focused").and_then(Value::as_bool) == Some(true) {
            return Some(node);
        }
        ["nodes", "floating_nodes"]
            .iter()
            .filter_map(|k| node.get(*k).and_then(Value::as_array))
            .flatten()
            .find_map(focused)
    }
    let tree: Value = serde_json::from_str(json).ok()?;
    let node = focused(&tree)?;
    let app_id = non_empty(node.get("app_id"))
        .or_else(|| non_empty(node.pointer("/window_properties/class")))?;
    Some(AppContext {
        app_id,
        title: non_empty(node.get("name")),
        source: "sway",
    })
}

/// Probe the focused application now. `None` when no probe applies to
/// this session or the probe failed.
pub fn snapshot() -> Option<AppContext> {
    if std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some() {
        return parse_hyprland(&run("hyprctl", &["activewindow", "-j"])?);
    }
    if std::env::var_os("SWAYSOCK").is_some() {
        return parse_sway(&run("swaymsg", &["-t", "get_tree", "-r"])?);
    }
    None
}

/// The snapshot taken at the latest record start.
static CAPTURED: Mutex<Option<AppContext>> = Mutex::new(None);

/// Daemon hook, called where every recording starts. Clears the previous
/// snapshot at once, then probes on a blocking thread so the recording
/// never waits on compositor IPC.
pub fn capture_at_record_start() {
    set_captured(None);
    if std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_none()
        && std::env::var_os("SWAYSOCK").is_none()
    {
        return;
    }
    tokio::task::spawn_blocking(|| {
        let ctx = snapshot();
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

    #[test]
    fn hyprland_active_window() {
        let json = r#"{"address":"0x5","class":"firefox","title":"Inbox - Mail","initialClass":"firefox","pid":42}"#;
        assert_eq!(
            parse_hyprland(json),
            Some(AppContext {
                app_id: "firefox".into(),
                title: Some("Inbox - Mail".into()),
                source: "hyprland"
            })
        );
    }

    #[test]
    fn hyprland_empty_workspace_and_garbage() {
        assert_eq!(parse_hyprland("{}"), None);
        assert_eq!(parse_hyprland("Invalid"), None);
        // class blank (some XWayland windows) falls back to initialClass
        let json = r#"{"class":"","initialClass":"steam","title":""}"#;
        let ctx = parse_hyprland(json).unwrap();
        assert_eq!(ctx.app_id, "steam");
        assert_eq!(ctx.title, None);
    }

    #[test]
    fn sway_focused_wayland_and_xwayland_nodes() {
        let tree = r#"{"focused":false,"nodes":[{"focused":false,"nodes":[
            {"focused":false,"app_id":"foot","name":"shell","nodes":[]},
            {"focused":false,"nodes":[],"floating_nodes":[
                {"focused":true,"app_id":"org.gnome.Nautilus","name":"Files","nodes":[]}
            ]}
        ]}]}"#;
        let ctx = parse_sway(tree).unwrap();
        assert_eq!(ctx.app_id, "org.gnome.Nautilus");
        assert_eq!(ctx.title.as_deref(), Some("Files"));
        assert_eq!(ctx.source, "sway");

        let xwayland = r#"{"focused":false,"nodes":[
            {"focused":true,"app_id":null,"name":"Slack","window_properties":{"class":"Slack"},"nodes":[]}
        ]}"#;
        assert_eq!(parse_sway(xwayland).unwrap().app_id, "Slack");
    }

    #[test]
    fn sway_nothing_focused() {
        assert_eq!(parse_sway(r#"{"focused":false,"nodes":[]}"#), None);
        // A focused workspace (no app) is not an app.
        assert_eq!(
            parse_sway(r#"{"focused":false,"nodes":[{"focused":true,"name":"1","nodes":[]}]}"#),
            None
        );
    }

    #[test]
    fn probe_failure_is_none() {
        assert_eq!(run("voxtype-definitely-not-a-command", &[]), None);
    }
}
