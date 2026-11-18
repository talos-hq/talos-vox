//! `voxtype history list|last|clear` and `voxtype paste-last`.

use std::path::Path;

use voxtype::history::{default_path, Entry, Store};
use voxtype::{config, output, HistoryAction};

fn open_store() -> anyhow::Result<Store> {
    let path = default_path();
    // Reading must not create a database for a user who never enabled
    // history: an in-memory store reads as empty.
    if !path.exists() {
        return Store::open(Path::new(":memory:")).map_err(Into::into);
    }
    Store::open(&path).map_err(|e| anyhow::anyhow!("cannot open {}: {e}", path.display()))
}

fn disabled_hint(config: &config::Config) {
    if !config.history.enabled {
        eprintln!("Note: history is off, so nothing new is being recorded.");
        eprintln!("Enable it with `[history] enabled = true` in config.toml.");
    }
}

fn timestamp(entry: &Entry) -> String {
    chrono::DateTime::from_timestamp_millis(entry.created_at_ms)
        .map(|t| {
            t.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M:%S")
                .to_string()
        })
        .unwrap_or_default()
}

pub(crate) fn run_history(config: &config::Config, action: HistoryAction) -> anyhow::Result<()> {
    let store = open_store()?;
    // Reading honours the current caps even if the daemon has not pruned yet.
    if config.history.enabled {
        store.prune(&config.history, chrono::Utc::now().timestamp_millis())?;
    }
    match action {
        HistoryAction::List { limit, json } => {
            let entries = store.list(limit)?;
            if json {
                println!("{}", serde_json::to_string(&entries)?);
            } else {
                for e in &entries {
                    println!("{}  {}", timestamp(e), e.text);
                }
                if entries.is_empty() {
                    eprintln!("No dictations in history.");
                    disabled_hint(config);
                }
            }
        }
        HistoryAction::Last { json } => match store.last()? {
            Some(e) if json => println!("{}", serde_json::to_string(&e)?),
            Some(e) => println!("{}", e.text),
            None => {
                eprintln!("No dictations in history.");
                disabled_hint(config);
                std::process::exit(3);
            }
        },
        HistoryAction::Clear => {
            let n = store.clear()?;
            println!("Removed {n} dictation(s) from history.");
        }
    }
    Ok(())
}

pub(crate) async fn run_paste_last(config: &config::Config) -> anyhow::Result<()> {
    let Some(entry) = open_store()?.last()? else {
        eprintln!("No dictations in history.");
        disabled_hint(config);
        std::process::exit(3);
    };
    let out = &config.output;
    let chain = output::create_output_chain(out);
    let options = output::OutputOptions {
        pre_output_command: out.pre_output_command.as_deref(),
        post_output_command: out.post_output_command.as_deref(),
        wait_for_modifier_release: out.wait_for_modifier_release,
        modifier_release_timeout: std::time::Duration::from_millis(out.modifier_release_timeout_ms),
    };
    output::output_with_fallback(&chain, &entry.text, options)
        .await
        .map_err(|e| anyhow::anyhow!("output failed: {e}"))?;
    Ok(())
}
