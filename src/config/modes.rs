//! `[[modes]]`: pick a profile from the application focused at record start.

use serde::{Deserialize, Serialize};

/// One `[[modes]]` entry. The first entry whose `app` or `title` matches the
/// app focused when recording started selects `profile`.
///
/// ```toml
/// [[modes]]
/// app = ["code", "com.microsoft.VSCode", "dev.zed.Zed"]
/// profile = "code"
///
/// [[modes]]
/// title = ["- Slack"]
/// profile = "chat"
/// ```
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct Mode {
    /// Optional label, used only in logs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    /// App identifiers to match exactly, ignoring case: the Wayland app_id
    /// or X11 class, macOS bundle id, or Windows exe stem. `voxtype context`
    /// prints the value for the focused window.
    #[serde(default)]
    pub app: Vec<String>,

    /// Substrings of the window title to match, ignoring case. Titles are
    /// not available on macOS.
    #[serde(default)]
    pub title: Vec<String>,

    /// Name of the `[profiles.NAME]` to apply.
    pub profile: String,
}
