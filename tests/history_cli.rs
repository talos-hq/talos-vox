//! `voxtype history` and the per-OS database location, end to end.

use std::path::Path;
use std::process::Command;

fn voxtype(home: &Path, config: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_voxtype"));
    cmd.arg("--config")
        .arg(config)
        .env("HOME", home)
        .env("XDG_RUNTIME_DIR", home.join("run"))
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("XDG_STATE_HOME", home.join("state"));
    cmd
}

#[cfg(target_os = "linux")]
#[test]
fn history_list_last_and_clear() {
    let home = tempfile::tempdir().unwrap();
    let config = home.path().join("config.toml");
    std::fs::write(
        &config,
        "[history]\nenabled = true\nmax_entries = 2\nretention_days = 0\n",
    )
    .unwrap();

    // Seed the database where the CLI will look for it on Linux.
    let db = home.path().join("state/voxtype/history.db");
    let store = voxtype::history::Store::open(&db).unwrap();
    store.insert(1, "first", "whisper", "base.en").unwrap();
    store.insert(2, "second", "whisper", "base.en").unwrap();
    store.insert(3, "third", "whisper", "base.en").unwrap();
    drop(store);

    // Reading applies max_entries = 2.
    let out = voxtype(home.path(), &config)
        .args(["history", "list", "--json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let entries: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let texts: Vec<_> = entries
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["text"].as_str().unwrap())
        .collect();
    assert_eq!(texts, ["third", "second"]);

    let out = voxtype(home.path(), &config)
        .args(["history", "last"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "third\n");

    let out = voxtype(home.path(), &config)
        .args(["history", "clear"])
        .output()
        .unwrap();
    assert!(out.status.success());

    // Empty history: `last` and `paste-last` exit 3 and type nothing.
    for args in [&["history", "last"][..], &["paste-last"][..]] {
        let out = voxtype(home.path(), &config).args(args).output().unwrap();
        assert_eq!(out.status.code(), Some(3), "{args:?}");
        assert!(out.stdout.is_empty(), "{args:?}");
    }
}
