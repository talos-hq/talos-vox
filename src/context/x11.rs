//! X11 sessions: `xprop` on `_NET_ACTIVE_WINDOW`.
//!
//! Only used when the session is X11. Under Wayland, `xprop` sees only
//! Xwayland windows, so it would report a stale or wrong app.

use super::{run, AppContext, FocusProbe};

pub(super) struct X11;

impl FocusProbe for X11 {
    fn name(&self) -> &'static str {
        "x11"
    }

    fn snapshot(&self) -> Option<AppContext> {
        let id = parse_active_window(&run("xprop", &["-root", "_NET_ACTIVE_WINDOW"])?)?;
        parse_window(&run("xprop", &["-id", &id, "WM_CLASS", "_NET_WM_NAME"])?)
    }
}

/// `_NET_ACTIVE_WINDOW(WINDOW): window id # 0x3a00007` -> `0x3a00007`.
/// No focus reads as id 0x0.
pub(super) fn parse_active_window(out: &str) -> Option<String> {
    let id = out.split('#').nth(1)?.split(',').next()?.trim();
    (id.starts_with("0x") && id != "0x0").then(|| id.to_string())
}

/// Read the class (second `WM_CLASS` string) and the UTF-8 title.
pub(super) fn parse_window(out: &str) -> Option<AppContext> {
    let mut class = None;
    let mut title = None;
    for line in out.lines() {
        let Some((key, value)) = line.split_once(" = ") else {
            continue;
        };
        let strings = quoted(value);
        if key.starts_with("WM_CLASS") {
            class = strings.into_iter().nth(1).filter(|s| !s.is_empty());
        } else if key.starts_with("_NET_WM_NAME") {
            title = strings.into_iter().next().filter(|s| !s.is_empty());
        }
    }
    Some(AppContext {
        app_id: class?,
        title,
        source: "x11",
    })
}

/// The double-quoted strings in an xprop value, with `\"` and `\\` unescaped.
fn quoted(value: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '"' {
            continue;
        }
        let mut s = String::new();
        while let Some(c) = chars.next() {
            match c {
                '\\' => {
                    if let Some(n) = chars.next() {
                        s.push(n);
                    }
                }
                '"' => break,
                c => s.push(c),
            }
        }
        out.push(s);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_window_id() {
        assert_eq!(
            parse_active_window("_NET_ACTIVE_WINDOW(WINDOW): window id # 0x3a00007\n"),
            Some("0x3a00007".into())
        );
        assert_eq!(
            parse_active_window("_NET_ACTIVE_WINDOW(WINDOW): window id # 0x0"),
            None
        );
        assert_eq!(parse_active_window("_NET_ACTIVE_WINDOW:  not found."), None);
    }

    #[test]
    fn class_and_title() {
        let out = "WM_CLASS(STRING) = \"Navigator\", \"firefox\"\n\
                   _NET_WM_NAME(UTF8_STRING) = \"Say \\\"hi\\\" - Mozilla Firefox\"\n";
        let ctx = parse_window(out).unwrap();
        assert_eq!(ctx.app_id, "firefox");
        assert_eq!(ctx.title.as_deref(), Some("Say \"hi\" - Mozilla Firefox"));
    }

    #[test]
    fn missing_class_is_no_app() {
        let out = "WM_CLASS:  not found.\n_NET_WM_NAME(UTF8_STRING) = \"x\"\n";
        assert_eq!(parse_window(out), None);
    }
}
