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
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

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
fn mcp_client_lists_and_calls_every_tool() {
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
        json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"dictate_start"}}),
        json!({"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"dictate_stop"}}),
        json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"history_list"}}),
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
    assert_eq!(replies.len(), 7, "replies: {replies:?}");

    assert_eq!(replies[0]["id"], 1);
    assert_eq!(replies[0]["result"]["serverInfo"]["name"], "voxtype");
    assert!(replies[0]["result"]["capabilities"]["tools"].is_object());

    let tools: Vec<&str> = replies[1]["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        tools,
        [
            "status",
            "transcribe_file",
            "dictate_start",
            "dictate_stop",
            "history_list"
        ]
    );

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

    // No daemon is running: dictation is refused as a tool error, and a
    // stop without a start says so.
    assert_eq!(replies[4]["result"]["isError"], true, "{}", replies[4]);
    assert_eq!(replies[5]["result"]["isError"], true, "{}", replies[5]);
    assert!(replies[5]["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("dictate_start"));

    // History is off in this config and the per-OS store is empty.
    let history = &replies[6]["result"]["structuredContent"];
    assert_eq!(
        history,
        &json!({"enabled": false, "entries": []}),
        "{}",
        replies[6]
    );
}

struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// dictate_start / dictate_stop against a real daemon. Runners have no
/// microphone or model, so the outcome is `empty` or `error`; what matters
/// is that the round trip completes well before the timeout and that the
/// text never went to the typing path.
#[test]
fn mcp_dictation_round_trip_through_the_daemon() {
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join("run")).unwrap();
    let config = home.path().join("config.toml");
    std::fs::write(
        &config,
        "state_file = \"auto\"\n\n\
         [hotkey]\nenabled = false\n\n\
         [osd]\nenabled = false\n\n\
         [whisper]\non_demand_loading = true\n",
    )
    .unwrap();

    let mut daemon = Daemon(
        voxtype(home.path(), &config)
            .arg("daemon")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let socket = home.path().join("run/voxtype/control.sock");
    let deadline = Instant::now() + Duration::from_secs(60);
    while voxtype::control::request(&socket, "status").is_err() {
        assert!(daemon.0.try_wait().unwrap().is_none(), "daemon exited");
        assert!(Instant::now() < deadline, "daemon never came up");
        std::thread::sleep(Duration::from_millis(200));
    }

    let mut child = voxtype(home.path(), &config)
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap()).lines();
    let mut call = |id: u64, name: &str, args: Value| -> Value {
        let req = json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":name,"arguments":args}});
        writeln!(stdin, "{req}").unwrap();
        let line = stdout.next().unwrap().unwrap();
        serde_json::from_str(&line).unwrap()
    };

    let started = call(1, "dictate_start", json!({}));
    assert_eq!(started["result"]["isError"], false, "{started}");
    let again = call(2, "dictate_start", json!({}));
    assert_eq!(
        again["result"]["isError"], true,
        "second start must be refused"
    );
    std::thread::sleep(Duration::from_millis(800));

    let begun = Instant::now();
    let stopped = call(3, "dictate_stop", json!({"timeout_secs": 30}));
    let result = &stopped["result"]["structuredContent"];
    assert!(
        matches!(result["status"].as_str(), Some("empty" | "error" | "ok")),
        "{stopped}"
    );
    assert!(begun.elapsed() < Duration::from_secs(30), "stop timed out");
    assert_eq!(result["engine"], "whisper");
    assert!(result["duration_ms"].as_u64().unwrap() >= 800);

    drop(stdin);
    assert!(child.wait().unwrap().success());
}
