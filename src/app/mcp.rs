//! `voxtype mcp`: binary-side tool bodies for the stdio MCP server in
//! `voxtype::mcp`. Each tool reuses the CLI command it mirrors.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::Instant;

use voxtype::{
    config,
    mcp::{Dictation, ToolHost, Transcript},
    status_json::{format_state_json, ExtendedStatusInfo},
};

use super::status::read_state;
use super::transcribe_file::transcribe_path;

struct Host {
    config: config::Config,
    /// `--config` given to `voxtype mcp`, forwarded to the `record` children.
    config_arg: Option<PathBuf>,
    session: Mutex<Option<Session>>,
}

/// A dictation between `dictate_start` and `dictate_stop`.
struct Session {
    transcript: PathBuf,
    started: Instant,
}

impl Host {
    /// `voxtype [--config X] record ...`, run as a child so its stdout (and
    /// its `process::exit`) stay out of this server's protocol stream.
    fn record(&self, args: &[&str]) -> Result<std::process::Output, String> {
        let exe = std::env::current_exe().map_err(|e| format!("cannot find voxtype: {e}"))?;
        let mut cmd = Command::new(exe);
        if let Some(path) = &self.config_arg {
            cmd.arg("--config").arg(path);
        }
        cmd.arg("record")
            .args(args)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("cannot run voxtype record: {e}"))
    }
}

fn stderr_of(output: &std::process::Output) -> String {
    let text = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if text.is_empty() {
        format!("voxtype record exited with {}", output.status)
    } else {
        text
    }
}

impl ToolHost for Host {
    fn status(&self) -> Result<String, String> {
        let state_path = self.config.resolve_state_file().ok_or_else(|| {
            "state_file is not configured; add `state_file = \"auto\"` to config.toml".to_string()
        })?;
        Ok(format_state_json(
            &read_state(&state_path),
            &self.config.status.resolve_icons(),
            Some(&ExtendedStatusInfo::from_config(&self.config)),
        ))
    }

    fn transcribe_file(&self, path: &Path, engine: Option<&str>) -> Result<Transcript, String> {
        let mut config = self.config.clone();
        if let Some(name) = engine {
            config.engine = name.parse::<config::TranscriptionEngine>().map_err(|_| {
                format!(
                    "invalid engine '{name}'. Valid options: {}",
                    voxtype::cli::ENGINE_NAMES_CSV
                )
            })?;
        }
        let outcome = transcribe_path(&config, path, &mut |_| {}).map_err(|e| format!("{e:#}"))?;
        Ok(Transcript {
            speech: outcome.text.is_some(),
            text: outcome.text.unwrap_or_default(),
            engine: config.engine.name().to_string(),
            duration_ms: (outcome.duration_secs * 1000.0).round() as u64,
        })
    }

    fn dictate_start(&self) -> Result<String, String> {
        let mut session = self.session.lock().map_err(|_| "session lock poisoned")?;
        if session.is_some() {
            return Err("a dictation is already in progress; call dictate_stop first".into());
        }
        let dir = config::Config::runtime_dir().join("mcp");
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        let transcript = dir.join(format!("dictation-{}.txt", std::process::id()));
        let _ = std::fs::remove_file(&transcript);

        // File output keeps the text out of the focused window; the two
        // --no-* flags keep the daemon from pressing Enter on its own.
        let file_arg = format!("--file={}", transcript.display());
        let output = self.record(&[
            "start",
            &file_arg,
            "--no-auto-submit",
            "--no-smart-auto-submit",
        ])?;
        if !output.status.success() {
            return Err(stderr_of(&output));
        }
        *session = Some(Session {
            transcript,
            started: Instant::now(),
        });
        Ok("Recording. Call dictate_stop to finish and get the text.".into())
    }

    fn dictate_stop(&self, timeout_secs: u64) -> Result<Dictation, String> {
        let session = self
            .session
            .lock()
            .map_err(|_| "session lock poisoned")?
            .take()
            .ok_or("no dictation in progress; call dictate_start first")?;
        let duration_ms = session.started.elapsed().as_millis() as u64;
        let wait_file = session.transcript.display().to_string();
        let timeout = timeout_secs.to_string();
        let output = self.record(&[
            "stop",
            "--wait",
            "--json",
            "--wait-file",
            &wait_file,
            "--timeout",
            &timeout,
        ]);
        let _ = std::fs::remove_file(&session.transcript);
        let output = output?;

        // `record stop --wait --json` prints its outcome even when it exits
        // non-zero (empty = 3, timeout = 4, error = 1).
        let stdout = String::from_utf8_lossy(&output.stdout);
        let outcome: serde_json::Value = stdout
            .lines()
            .rev()
            .find_map(|line| serde_json::from_str(line).ok())
            .ok_or_else(|| stderr_of(&output))?;
        Ok(Dictation {
            status: outcome["status"].as_str().unwrap_or("error").to_string(),
            text: outcome["text"].as_str().unwrap_or_default().to_string(),
            message: outcome["message"].as_str().map(str::to_string),
            engine: self.config.engine.name().to_string(),
            model: self.config.model_name().to_string(),
            language: self.config.active_language().map(str::to_string),
            duration_ms,
        })
    }
}

/// Serve MCP on stdin/stdout until stdin closes.
pub(crate) async fn run_mcp(
    config: config::Config,
    config_arg: Option<PathBuf>,
) -> anyhow::Result<()> {
    // Transcription and the blocking stdin loop must not run on a runtime
    // worker: some engines block on their own HTTP clients.
    tokio::task::spawn_blocking(move || {
        let host = Host {
            config,
            config_arg,
            session: Mutex::new(None),
        };
        voxtype::mcp::serve(&host, std::io::stdin().lock(), std::io::stdout().lock())
    })
    .await??;
    Ok(())
}
