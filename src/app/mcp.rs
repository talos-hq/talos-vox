//! `voxtype mcp`: binary-side tool bodies for the stdio MCP server in
//! `voxtype::mcp`. Each tool reuses the CLI command it mirrors.

use std::path::Path;

use voxtype::{
    config,
    mcp::{ToolHost, Transcript},
    status_json::{format_state_json, ExtendedStatusInfo},
};

use super::status::read_state;
use super::transcribe_file::transcribe_path;

struct Host {
    config: config::Config,
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
}

/// Serve MCP on stdin/stdout until stdin closes.
pub(crate) async fn run_mcp(config: config::Config) -> anyhow::Result<()> {
    // Transcription and the blocking stdin loop must not run on a runtime
    // worker: some engines block on their own HTTP clients.
    tokio::task::spawn_blocking(move || {
        let host = Host { config };
        voxtype::mcp::serve(&host, std::io::stdin().lock(), std::io::stdout().lock())
    })
    .await??;
    Ok(())
}
