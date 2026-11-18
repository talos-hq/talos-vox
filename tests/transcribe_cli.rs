//! `voxtype transcribe` keeps stdout for the transcript (#4).
//!
//! Callers such as clud's F3 voice mode capture stdout, so progress lines and
//! logs must go to stderr, with or without `-q`. These runs need no model and
//! no GPU: one stops at VAD (no speech), the other fails to find its model.

use std::path::Path;
use std::process::{Command, Output};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/vad");

fn transcribe(home: &Path, config_toml: &str, args: &[&str], wav: &str) -> Output {
    let config = home.join("config.toml");
    std::fs::write(&config, config_toml).unwrap();
    Command::new(env!("CARGO_BIN_EXE_voxtype"))
        .arg("--config")
        .arg(&config)
        .args(args)
        .arg("transcribe")
        .arg(Path::new(FIXTURES).join(wav))
        .env("HOME", home)
        .env("XDG_RUNTIME_DIR", home.join("run"))
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("XDG_STATE_HOME", home.join("state"))
        .env_remove("RUST_LOG")
        .output()
        .unwrap()
}

const VAD_CONFIG: &str = "[vad]\nenabled = true\nbackend = \"energy\"\n";

#[test]
fn no_speech_leaves_stdout_empty_and_reports_on_stderr() {
    let home = tempfile::tempdir().unwrap();
    let out = transcribe(home.path(), VAD_CONFIG, &[], "silence_2s.wav");
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(out.status.success(), "stderr: {stderr}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "");
    assert!(stderr.contains("Loading audio file"), "stderr: {stderr}");
    assert!(stderr.contains("No speech detected"), "stderr: {stderr}");
}

#[test]
fn quiet_silences_progress_and_stdout_stays_empty() {
    let home = tempfile::tempdir().unwrap();
    let out = transcribe(home.path(), VAD_CONFIG, &["-q"], "silence_2s.wav");

    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout), "");
    assert_eq!(String::from_utf8_lossy(&out.stderr), "");
}

#[test]
fn init_failure_exits_nonzero_with_error_on_stderr_only() {
    let home = tempfile::tempdir().unwrap();
    let config = "engine = \"whisper\"\n[whisper]\nmodel = \"/nonexistent/ggml-missing.bin\"\n";
    for args in [&[][..], &["-q"][..]] {
        let out = transcribe(home.path(), config, args, "speech_hello.wav");
        let stderr = String::from_utf8_lossy(&out.stderr);

        assert_eq!(
            out.status.code(),
            Some(1),
            "args {args:?}, stderr: {stderr}"
        );
        assert_eq!(String::from_utf8_lossy(&out.stdout), "", "args {args:?}");
        assert!(stderr.contains("Error:"), "args {args:?}, stderr: {stderr}");
    }
}
