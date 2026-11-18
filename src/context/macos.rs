//! macOS: the frontmost application's bundle identifier, via `lsappinfo`.
//!
//! `NSWorkspace.frontmostApplication` is the obvious API, but it is only
//! refreshed by the main run loop, which this daemon does not run, so it
//! goes stale after the first read. `lsappinfo` asks the launch services
//! daemon directly, needs no TCC permission, and costs a few milliseconds.
//! Window titles are not read: on macOS they need Screen Recording.

use super::{run, AppContext, FocusProbe};

pub(super) struct MacOs;

impl FocusProbe for MacOs {
    fn name(&self) -> &'static str {
        "macos"
    }

    fn snapshot(&self) -> Option<AppContext> {
        let asn = run("lsappinfo", &["front"])?;
        let asn = asn.trim();
        if asn.is_empty() {
            return None;
        }
        let info = run("lsappinfo", &["info", "-only", "bundleid", asn])?;
        Some(AppContext {
            app_id: parse_bundle_id(&info)?,
            title: None,
            source: "macos",
        })
    }
}

/// `"CFBundleIdentifier"="com.apple.Terminal"` -> `com.apple.Terminal`.
/// Apps without a bundle print `[ NULL ]` and yield `None`.
pub(super) fn parse_bundle_id(out: &str) -> Option<String> {
    let (_, value) = out.split_once('=')?;
    let value = value.trim().trim_matches('"');
    (!value.is_empty() && !value.contains("NULL")).then(|| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_id() {
        assert_eq!(
            parse_bundle_id("\"CFBundleIdentifier\"=\"com.microsoft.VSCode\"\n"),
            Some("com.microsoft.VSCode".into())
        );
        assert_eq!(parse_bundle_id("\"CFBundleIdentifier\"=[ NULL ]\n"), None);
        assert_eq!(parse_bundle_id(""), None);
    }
}
