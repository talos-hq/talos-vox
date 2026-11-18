//! Opt-in dictation history: a small SQLite log of finished dictations.
//!
//! Off unless `[history] enabled = true`. Only the final text is stored
//! (after replacements, post-processing and `redact`), with the engine and
//! model that produced it. Audio is never stored. The log is capped by
//! `max_entries` and `retention_days` and pruned when the daemon starts and
//! at most hourly after that.
//!
//! The database is regenerable state, so it lives in the per-OS state dir:
//! - Linux: `$XDG_STATE_HOME/voxtype/history.db` (`~/.local/state/...`)
//! - macOS: `~/Library/Application Support/voxtype/history.db`
//! - Windows: `%LOCALAPPDATA%\voxtype\history.db`

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Mutex;

use rusqlite::{params, Connection};

use crate::config::{Config, HistoryConfig};

/// One stored dictation.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Entry {
    pub id: i64,
    /// Unix time in milliseconds.
    pub created_at_ms: i64,
    pub text: String,
    pub engine: String,
    pub model: String,
}

const MS_PER_DAY: i64 = 24 * 60 * 60 * 1000;
const PRUNE_INTERVAL_MS: i64 = 60 * 60 * 1000;

/// Where the history database lives on this OS.
pub fn default_path() -> PathBuf {
    dirs::state_dir()
        .or_else(dirs::data_local_dir)
        .unwrap_or_else(std::env::temp_dir)
        .join("voxtype")
        .join("history.db")
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Apply `redact` patterns. Invalid patterns are skipped with a warning
/// rather than failing the dictation.
pub fn redact(config: &HistoryConfig, text: &str) -> String {
    let mut out = text.to_string();
    for pattern in &config.redact {
        match regex::Regex::new(pattern) {
            Ok(re) => out = re.replace_all(&out, "[redacted]").into_owned(),
            Err(e) => tracing::warn!("Ignoring invalid [history] redact pattern {pattern:?}: {e}"),
        }
    }
    out
}

/// An open history database.
pub struct Store {
    conn: Connection,
}

impl Store {
    /// Open (creating if needed) the database at `path`.
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS dictations (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 created_at_ms INTEGER NOT NULL,
                 text TEXT NOT NULL,
                 engine TEXT NOT NULL DEFAULT '',
                 model TEXT NOT NULL DEFAULT ''
             );
             CREATE INDEX IF NOT EXISTS dictations_created ON dictations(created_at_ms);",
        )?;
        restrict_permissions(path);
        Ok(Self { conn })
    }

    pub fn insert(
        &self,
        at_ms: i64,
        text: &str,
        engine: &str,
        model: &str,
    ) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT INTO dictations (created_at_ms, text, engine, model) VALUES (?1, ?2, ?3, ?4)",
            params![at_ms, text, engine, model],
        )?;
        Ok(())
    }

    /// Enforce `retention_days` (relative to `now_ms`) and `max_entries`.
    /// Returns how many entries were removed.
    pub fn prune(&self, config: &HistoryConfig, now_ms: i64) -> rusqlite::Result<usize> {
        let mut removed = 0;
        if config.retention_days > 0 {
            let cutoff = now_ms - i64::from(config.retention_days) * MS_PER_DAY;
            removed += self.conn.execute(
                "DELETE FROM dictations WHERE created_at_ms < ?1",
                params![cutoff],
            )?;
        }
        removed += self.conn.execute(
            "DELETE FROM dictations WHERE id NOT IN
                 (SELECT id FROM dictations ORDER BY created_at_ms DESC, id DESC LIMIT ?1)",
            params![i64::from(config.max_entries)],
        )?;
        Ok(removed)
    }

    /// Newest first, at most `limit` entries.
    pub fn list(&self, limit: usize) -> rusqlite::Result<Vec<Entry>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, created_at_ms, text, engine, model FROM dictations
             ORDER BY created_at_ms DESC, id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |row| {
            Ok(Entry {
                id: row.get(0)?,
                created_at_ms: row.get(1)?,
                text: row.get(2)?,
                engine: row.get(3)?,
                model: row.get(4)?,
            })
        })?;
        rows.collect()
    }

    pub fn last(&self) -> rusqlite::Result<Option<Entry>> {
        Ok(self.list(1)?.into_iter().next())
    }

    /// Delete every entry. Returns how many were removed.
    pub fn clear(&self) -> rusqlite::Result<usize> {
        let n = self.conn.execute("DELETE FROM dictations", [])?;
        let _ = self.conn.execute_batch("VACUUM");
        Ok(n)
    }
}

/// History holds what the user said: keep it private to them.
fn restrict_permissions(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// Serializes daemon-side writes, and remembers when we last pruned.
static WRITE_LOCK: Mutex<()> = Mutex::new(());
static LAST_PRUNE_MS: AtomicI64 = AtomicI64::new(0);

fn write_and_prune(history: &HistoryConfig, path: &Path, entry: Option<(&str, &str, &str)>) {
    let _guard = WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let store = match Store::open(path) {
        Ok(store) => store,
        Err(e) => {
            tracing::warn!("History: cannot open {}: {e}", path.display());
            return;
        }
    };
    let now = now_ms();
    if let Some((text, engine, model)) = entry {
        if let Err(e) = store.insert(now, text, engine, model) {
            tracing::warn!("History: insert failed: {e}");
        }
    }
    if now - LAST_PRUNE_MS.load(Ordering::Relaxed) >= PRUNE_INTERVAL_MS {
        match store.prune(history, now) {
            Ok(n) if n > 0 => tracing::debug!("History: pruned {n} entries"),
            Ok(_) => {}
            Err(e) => tracing::warn!("History: prune failed: {e}"),
        }
        LAST_PRUNE_MS.store(now, Ordering::Relaxed);
    }
}

/// Daemon hook: record a finished dictation. Returns immediately; the write
/// happens on a blocking thread. Does nothing unless history is enabled.
pub fn record_dictation(config: &Config, text: &str) {
    if !config.history.enabled || text.trim().is_empty() {
        return;
    }
    let history = config.history.clone();
    let text = redact(&history, text);
    let engine = config.engine.name().to_string();
    let model = config.model_name().to_string();
    let path = default_path();
    tokio::task::spawn_blocking(move || {
        write_and_prune(&history, &path, Some((&text, &engine, &model)));
    });
}

/// Daemon hook: prune once at startup. Does nothing unless history is
/// enabled.
pub fn prune_on_start(config: &Config) {
    if !config.history.enabled {
        return;
    }
    let history = config.history.clone();
    let path = default_path();
    tokio::task::spawn_blocking(move || write_and_prune(&history, &path, None));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("history.db")).unwrap();
        (dir, store)
    }

    #[test]
    fn off_by_default() {
        let config = Config::default();
        assert!(!config.history.enabled);
        assert_eq!(config.history.retention_days, 7);
        assert_eq!(config.history.max_entries, 500);
    }

    #[test]
    fn list_is_newest_first_and_last_is_newest() {
        let (_d, s) = store();
        s.insert(1_000, "one", "whisper", "base.en").unwrap();
        s.insert(3_000, "three", "whisper", "base.en").unwrap();
        s.insert(2_000, "two", "parakeet", "tdt").unwrap();
        let texts: Vec<_> = s.list(10).unwrap().into_iter().map(|e| e.text).collect();
        assert_eq!(texts, ["three", "two", "one"]);
        assert_eq!(s.list(2).unwrap().len(), 2);
        assert_eq!(s.last().unwrap().unwrap().text, "three");
    }

    #[test]
    fn prune_enforces_retention_days() {
        let (_d, s) = store();
        let now = 100 * MS_PER_DAY;
        s.insert(now - 8 * MS_PER_DAY, "too old", "w", "m").unwrap();
        s.insert(now - 6 * MS_PER_DAY, "recent", "w", "m").unwrap();
        s.insert(now, "now", "w", "m").unwrap();
        let config = HistoryConfig::default(); // 7 days
        assert_eq!(s.prune(&config, now).unwrap(), 1);
        let texts: Vec<_> = s.list(10).unwrap().into_iter().map(|e| e.text).collect();
        assert_eq!(texts, ["now", "recent"]);
    }

    #[test]
    fn prune_enforces_max_entries_keeping_the_newest() {
        let (_d, s) = store();
        for i in 0..10 {
            s.insert(i, &format!("e{i}"), "w", "m").unwrap();
        }
        let config = HistoryConfig {
            max_entries: 3,
            retention_days: 0,
            ..HistoryConfig::default()
        };
        assert_eq!(s.prune(&config, 10).unwrap(), 7);
        let texts: Vec<_> = s.list(10).unwrap().into_iter().map(|e| e.text).collect();
        assert_eq!(texts, ["e9", "e8", "e7"]);
    }

    #[test]
    fn retention_zero_keeps_old_entries() {
        let (_d, s) = store();
        s.insert(0, "ancient", "w", "m").unwrap();
        let config = HistoryConfig {
            retention_days: 0,
            ..HistoryConfig::default()
        };
        assert_eq!(s.prune(&config, 1_000 * MS_PER_DAY).unwrap(), 0);
    }

    #[test]
    fn clear_removes_everything() {
        let (_d, s) = store();
        s.insert(1, "a", "w", "m").unwrap();
        s.insert(2, "b", "w", "m").unwrap();
        assert_eq!(s.clear().unwrap(), 2);
        assert!(s.last().unwrap().is_none());
    }

    #[test]
    fn redaction_applies_patterns_and_skips_invalid_ones() {
        let config = HistoryConfig {
            redact: vec![r"\b\d{4}\b".into(), "(unclosed".into(), "secret".into()],
            ..HistoryConfig::default()
        };
        assert_eq!(
            redact(&config, "pin 1234 is secret"),
            "pin [redacted] is [redacted]"
        );
    }

    #[cfg(unix)]
    #[test]
    fn database_is_private_to_the_user() {
        use std::os::unix::fs::PermissionsExt;
        let (d, _s) = store();
        let mode = std::fs::metadata(d.path().join("history.db"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
