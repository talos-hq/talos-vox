//! Hyprland: `hyprctl activewindow -j`.

use super::{non_empty, run, AppContext, FocusProbe};
use serde_json::Value;

pub(super) struct Hyprland;

impl FocusProbe for Hyprland {
    fn name(&self) -> &'static str {
        "hyprland"
    }

    fn snapshot(&self) -> Option<AppContext> {
        parse(&run("hyprctl", &["activewindow", "-j"])?)
    }
}

/// Parse `hyprctl activewindow -j`. An empty workspace answers `{}`.
pub(super) fn parse(json: &str) -> Option<AppContext> {
    let v: Value = serde_json::from_str(json).ok()?;
    let app_id = non_empty(v.get("class")).or_else(|| non_empty(v.get("initialClass")))?;
    Some(AppContext {
        app_id,
        title: non_empty(v.get("title")),
        source: "hyprland",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_window() {
        let json = r#"{"address":"0x5","class":"firefox","title":"Inbox - Mail","initialClass":"firefox","pid":42}"#;
        assert_eq!(
            parse(json),
            Some(AppContext {
                app_id: "firefox".into(),
                title: Some("Inbox - Mail".into()),
                source: "hyprland"
            })
        );
    }

    #[test]
    fn empty_workspace_and_garbage() {
        assert_eq!(parse("{}"), None);
        assert_eq!(parse("Invalid"), None);
        // class blank (some XWayland windows) falls back to initialClass
        let json = r#"{"class":"","initialClass":"steam","title":""}"#;
        let ctx = parse(json).unwrap();
        assert_eq!(ctx.app_id, "steam");
        assert_eq!(ctx.title, None);
    }
}
