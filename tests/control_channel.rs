//! End-to-end control protocol: start the real daemon binary, drive
//! `start`/`stop` over its control channel and read JSON status back.
//!
//! No model is loaded (on-demand loading) and runners usually have no
//! microphone, so this asserts the protocol and that the daemon survives
//! the requests, not transcription.

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use voxtype::control;

struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn status_until_ready(socket: &Path, daemon: &mut Daemon) -> serde_json::Value {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        match control::request(socket, "status") {
            Ok(reply) => return reply,
            Err(e) => {
                if let Some(exit) = daemon.0.try_wait().unwrap() {
                    panic!("daemon exited before serving control requests: {exit}");
                }
                assert!(
                    Instant::now() < deadline,
                    "control channel never came up: {e}"
                );
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }
}

#[test]
fn record_start_stop_and_status_over_the_control_channel() {
    let home = tempfile::tempdir().unwrap();
    let xdg_runtime = home.path().join("run");
    std::fs::create_dir_all(&xdg_runtime).unwrap();
    let config = home.path().join("config.toml");
    std::fs::write(
        &config,
        "state_file = \"auto\"\n\n\
         [hotkey]\nenabled = false\n\n\
         [osd]\nenabled = false\n\n\
         [whisper]\non_demand_loading = true\n\n\
         [output]\nmode = \"file\"\n",
    )
    .unwrap();

    let mut daemon = Daemon(
        Command::new(env!("CARGO_BIN_EXE_voxtype"))
            .arg("--config")
            .arg(&config)
            .arg("daemon")
            .env("HOME", home.path())
            .env("XDG_RUNTIME_DIR", &xdg_runtime)
            .env("XDG_CONFIG_HOME", home.path().join("config"))
            .env("XDG_DATA_HOME", home.path().join("data"))
            .env("XDG_CACHE_HOME", home.path().join("cache"))
            .stdin(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let socket = xdg_runtime.join("voxtype").join("control.sock");

    let ready = status_until_ready(&socket, &mut daemon);
    assert_eq!(ready["ok"], true, "status reply: {ready}");
    assert_eq!(ready["state"], "idle", "status reply: {ready}");

    for request in ["start", "status", "stop", "status"] {
        let reply = control::request(&socket, request).unwrap();
        assert_eq!(reply["ok"], true, "{request} reply: {reply}");
        assert!(reply["state"].is_string(), "{request} reply: {reply}");
        std::thread::sleep(Duration::from_millis(300));
    }

    // The CLI reaches the same daemon: signals on Unix, the control
    // channel elsewhere.
    for action in ["start", "stop"] {
        let output = Command::new(env!("CARGO_BIN_EXE_voxtype"))
            .arg("--config")
            .arg(&config)
            .args(["record", action])
            .env("HOME", home.path())
            .env("XDG_RUNTIME_DIR", &xdg_runtime)
            .env("XDG_CONFIG_HOME", home.path().join("config"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "voxtype record {action} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        std::thread::sleep(Duration::from_millis(300));
    }

    let unknown = control::request(&socket, "bogus").unwrap();
    assert_eq!(unknown["ok"], false, "bogus reply: {unknown}");

    assert!(
        daemon.0.try_wait().unwrap().is_none(),
        "daemon must survive start/stop without a model or microphone"
    );
}
