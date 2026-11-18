//! Sway: the focused node of `swaymsg -t get_tree`.

use super::{non_empty, run, AppContext, FocusProbe};
use serde_json::Value;

pub(super) struct Sway;

impl FocusProbe for Sway {
    fn name(&self) -> &'static str {
        "sway"
    }

    fn snapshot(&self) -> Option<AppContext> {
        parse(&run("swaymsg", &["-t", "get_tree", "-r"])?)
    }
}

/// Parse `swaymsg -t get_tree`: the focused leaf's `app_id` (Wayland) or
/// `window_properties.class` (Xwayland).
pub(super) fn parse(json: &str) -> Option<AppContext> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focused_wayland_and_xwayland_nodes() {
        let tree = r#"{"focused":false,"nodes":[{"focused":false,"nodes":[
            {"focused":false,"app_id":"foot","name":"shell","nodes":[]},
            {"focused":false,"nodes":[],"floating_nodes":[
                {"focused":true,"app_id":"org.gnome.Nautilus","name":"Files","nodes":[]}
            ]}
        ]}]}"#;
        let ctx = parse(tree).unwrap();
        assert_eq!(ctx.app_id, "org.gnome.Nautilus");
        assert_eq!(ctx.title.as_deref(), Some("Files"));
        assert_eq!(ctx.source, "sway");

        let xwayland = r#"{"focused":false,"nodes":[
            {"focused":true,"app_id":null,"name":"Slack","window_properties":{"class":"Slack"},"nodes":[]}
        ]}"#;
        assert_eq!(parse(xwayland).unwrap().app_id, "Slack");
    }

    #[test]
    fn nothing_focused() {
        assert_eq!(parse(r#"{"focused":false,"nodes":[]}"#), None);
        // A focused workspace (no app) is not an app.
        assert_eq!(
            parse(r#"{"focused":false,"nodes":[{"focused":true,"name":"1","nodes":[]}]}"#),
            None
        );
    }
}
