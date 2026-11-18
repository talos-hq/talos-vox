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

/// Write a whisper.cpp model whose dimensions need far more memory than any
/// machine has, so allocating the model buffer fails, followed by one small
/// tensor (`decoder.ln.bias`, 1 MiB). Stock whisper.cpp skips the buffer it
/// couldn't allocate, then aborts on `GGML_ASSERT(buffer)` when it copies
/// that tensor in: the same abort as a model that doesn't fit on the GPU.
///
/// The context lengths are huge too, so that every other allocation on that
/// path (the KV cache) also fails at once rather than asking for a few tens
/// of gigabytes that a big machine might actually hand out.
fn write_oversized_whisper_model(path: &Path) {
    const GGML_FILE_MAGIC: u32 = 0x6767_6d6c;
    let n_state: i32 = 1 << 18;
    let n_ctx: i32 = 1 << 26;
    let hparams: [i32; 11] = [
        51864,   // n_vocab
        n_ctx,   // n_audio_ctx
        n_state, // n_audio_state
        6,       // n_audio_head
        4,       // n_audio_layer
        n_ctx,   // n_text_ctx
        n_state, // n_text_state
        6,       // n_text_head
        4,       // n_text_layer
        80,      // n_mels
        1,       // ftype: f16
    ];
    let name = b"decoder.ln.bias";
    let mut bytes = GGML_FILE_MAGIC.to_le_bytes().to_vec();
    // hparams; an empty mel filter bank (n_mel, n_fft) and vocabulary
    // (n_vocab); then one f32 tensor header (n_dims, name length, type,
    // ne[0]).
    let header = hparams
        .into_iter()
        .chain([0, 0, 0])
        .chain([1, name.len() as i32, 0, n_state]);
    for v in header {
        bytes.extend(v.to_le_bytes());
    }
    bytes.extend(name);
    bytes.resize(bytes.len() + 4 * n_state as usize, 0);
    std::fs::write(path, bytes).unwrap();
}

/// A Whisper model that doesn't fit in memory is a clean error, not an abort.
///
/// Stock whisper.cpp skips a model buffer it couldn't allocate and then
/// aborts on `GGML_ASSERT(buffer)` (exit 134, #4). The patched copy fails the
/// load, which is what lets a GPU build retry on CPU. This build has no GPU backend, so the CPU load is the one
/// that fails, and the error must reach stderr with exit status 1.
#[test]
fn whisper_model_too_big_for_memory_is_a_clean_error() {
    let home = tempfile::tempdir().unwrap();
    let model = home.path().join("ggml-oversized.bin");
    write_oversized_whisper_model(&model);
    let config = format!(
        "engine = \"whisper\"\n[whisper]\nmodel = \"{}\"\n",
        model.display()
    );
    let out = transcribe(home.path(), &config, &[], "speech_hello.wav");
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert_eq!(out.status.code(), Some(1), "stderr: {stderr}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "");
    assert!(
        stderr.contains("could not load the model"),
        "stderr: {stderr}"
    );
}
