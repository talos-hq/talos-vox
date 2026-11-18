//! Dictation history configuration (`[history]`). Off by default.

use serde::{Deserialize, Serialize};

/// `[history]`: an opt-in, capped local log of finished dictations, used by
/// `voxtype history` and `voxtype paste-last`. Only text is stored, never
/// audio.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct HistoryConfig {
    /// Record each dictation's final text (default: false).
    #[serde(default)]
    pub enabled: bool,

    /// Drop entries older than this many days (default: 7, 0 = keep until
    /// `max_entries` pushes them out).
    #[serde(default = "default_retention_days")]
    pub retention_days: u32,

    /// Keep at most this many entries, newest first (default: 500).
    #[serde(default = "default_max_entries")]
    pub max_entries: u32,

    /// Regular expressions whose matches are replaced with `[redacted]`
    /// before an entry is stored. Applied after replacements and
    /// post-processing, so they see the text that was actually output.
    #[serde(default)]
    pub redact: Vec<String>,
}

fn default_retention_days() -> u32 {
    7
}

fn default_max_entries() -> u32 {
    500
}

impl Default for HistoryConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            retention_days: default_retention_days(),
            max_entries: default_max_entries(),
            redact: Vec::new(),
        }
    }
}
