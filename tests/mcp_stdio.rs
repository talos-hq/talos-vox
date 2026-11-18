//! `voxtype mcp` end to end: launch the real binary the way an agent host
//! does, run the MCP handshake over stdio, list the tools and call both.
//!
//! Runners have no model, so `transcribe_file` is asserted to reach the
//! transcriber and report its failure as a tool error, not to transcribe.
//! `status` is asserted byte-equal to `voxtype status --format json
//! --extended` in the same environment.
#![cfg(feature = "mcp")]

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};

use serde_json::{json, Value};

fn voxtype(home: &Path, config: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_voxtype"));
    cmd.arg("--config")
        .arg(config)
        .env("HOME", home)
        .env("XDG_RUNTIME_DIR", home.join("run"))
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("XDG_CACHE_HOME", home.join("cache"))
        .env("XDG_STATE_HOME", home.join("state"));
    cmd
}

#[test]
fn mcp_client_lists_and_calls_both_tools() {
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join("run")).unwrap();
    let config = home.path().join("config.toml");
    std::fs::write(&config, "state_file = \"auto\"\n").unwrap();
    let wav = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vad/speech_hello.wav");

    let mut child = voxtype(home.path(), &config)
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let requests = [
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"0"}}}),
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
        json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"status","arguments":{}}}),
        json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"transcribe_file","arguments":{"path":wav}}}),
    ];
    for r in &requests {
        writeln!(stdin, "{r}").unwrap();
    }
    drop(stdin); // EOF ends the session.

    // Every stdout line must be a JSON-RPC message: a stray log line
    // would corrupt the stream for a real client.
    let replies: Vec<Value> = BufReader::new(child.stdout.take().unwrap())
        .lines()
        .map(|l| {
            let l = l.unwrap();
            serde_json::from_str(&l).unwrap_or_else(|e| panic!("non-JSON on stdout: {l:?}: {e}"))
        })
        .collect();
    let status = child.wait().unwrap();
    assert!(status.success(), "voxtype mcp exited with {status}");
    assert_eq!(replies.len(), 4, "replies: {replies:?}");

    assert_eq!(replies[0]["id"], 1);
    assert_eq!(replies[0]["result"]["serverInfo"]["name"], "voxtype");
    assert!(replies[0]["result"]["capabilities"]["tools"].is_object());

    let tools: Vec<&str> = replies[1]["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(tools, ["status", "transcribe_file"]);

    let mcp_status = &replies[2]["result"];
    assert_eq!(mcp_status["isError"], false, "{mcp_status}");
    let cli = voxtype(home.path(), &config)
        .args(["status", "--format", "json", "--extended"])
        .output()
        .unwrap();
    assert!(cli.status.success());
    let cli_json = String::from_utf8(cli.stdout).unwrap();
    assert_eq!(
        mcp_status["content"][0]["text"].as_str().unwrap(),
        cli_json.trim_end(),
        "MCP status must match `voxtype status --format json --extended`"
    );
    assert_eq!(mcp_status["structuredContent"]["class"], "stopped");

    // No model is installed, so the call reaches the transcriber and fails
    // there: reported as a tool error, and the server keeps running.
    let transcribe = &replies[3];
    assert_eq!(transcribe["id"], 4);
    assert!(transcribe.get("error").is_none(), "{transcribe}");
    assert_eq!(transcribe["result"]["isError"], true, "{transcribe}");
}
