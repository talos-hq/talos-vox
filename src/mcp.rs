//! `voxtype mcp`: a Model Context Protocol server over stdio.
//!
//! The server runs in the CLI process, never in the daemon, and reads
//! newline-delimited JSON-RPC 2.0 from stdin and writes replies to stdout.
//! Nothing listens on a socket or port. Logs must go to stderr, because
//! stdout carries the protocol.
//!
//! The tools are thin peers of existing CLI commands, so they cannot drift
//! from them:
//! - `status`: the `voxtype status --format json --extended` payload.
//! - `transcribe_file`: `voxtype transcribe <file>`, returning the text.
//! - `dictate_start` / `dictate_stop`: a live dictation through the running
//!   daemon, returned to the caller instead of typed. Built on the
//!   `record start --file` / `record stop --wait --json` contract
//!   (docs/INTEGRATIONS.md), so the daemon needs no new verb.
//!
//! The protocol layer lives here; the binary supplies the tool bodies
//! through [`ToolHost`], so this file stays testable without a model.

use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

/// MCP revision this server implements. A client asking for another
/// revision gets this one back, and decides whether to continue (per the
/// spec's version negotiation).
pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// The result of a `transcribe_file` call.
#[derive(Debug, Clone, PartialEq)]
pub struct Transcript {
    /// Processed text, or empty when VAD found no speech.
    pub text: String,
    /// Engine that produced it, as named in config (`whisper`, `parakeet`, ...).
    pub engine: String,
    /// Audio length in milliseconds, after resampling to 16 kHz.
    pub duration_ms: u64,
    /// False when VAD skipped transcription because the file had no speech.
    pub speech: bool,
}

/// Tool implementations, supplied by the binary.
pub trait ToolHost {
    /// The `status --format json --extended` line, exactly as the CLI
    /// prints it (without the newline).
    fn status(&self) -> Result<String, String>;
    /// Transcribe a WAV file, optionally with another engine.
    fn transcribe_file(&self, path: &Path, engine: Option<&str>) -> Result<Transcript, String>;
    /// Start recording on the daemon, capturing the text for `dictate_stop`.
    /// Returns a short human-readable confirmation.
    fn dictate_start(&self) -> Result<String, String>;
    /// Stop the recording `dictate_start` began and wait up to
    /// `timeout_secs` for its transcript.
    fn dictate_stop(&self, timeout_secs: u64) -> Result<Dictation, String>;
}

/// The result of `dictate_stop`.
#[derive(Debug, Clone, PartialEq)]
pub struct Dictation {
    /// `ok`, `empty` (no speech, or too short), `timeout` or `error`, as
    /// reported by `voxtype record stop --wait --json`.
    pub status: String,
    /// Processed transcript; empty unless `status` is `ok`.
    pub text: String,
    /// Daemon-side failure detail for `error`.
    pub message: Option<String>,
    pub engine: String,
    pub model: String,
    /// Configured language code, or `None` for auto-detect.
    pub language: Option<String>,
    /// Wall time from `dictate_start` to `dictate_stop`.
    pub duration_ms: u64,
}

/// Default and upper bound for `dictate_stop`'s wait, in seconds.
pub const DICTATE_STOP_TIMEOUT_SECS: u64 = 120;

fn tool_list() -> Value {
    json!([
        {
            "name": "status",
            "title": "Voxtype daemon status",
            "description": "Current voxtype daemon state (idle, recording, transcribing, stopped, ...) \
                            with model, device and backend. Same object as `voxtype status --format json --extended`.",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
            "annotations": { "readOnlyHint": true, "openWorldHint": false }
        },
        {
            "name": "transcribe_file",
            "title": "Transcribe an audio file",
            "description": "Transcribe a WAV file with the configured engine and text pipeline \
                            (replacements, spoken punctuation). Same as `voxtype transcribe <file>`. \
                            Runs in this process; the daemon is not involved and need not be running.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Path to a WAV file. Relative paths resolve against the server's working directory." },
                    "engine": { "type": "string", "description": "Override the configured engine for this call (e.g. whisper, parakeet)." }
                },
                "required": ["path"],
                "additionalProperties": false
            },
            "outputSchema": {
                "type": "object",
                "properties": {
                    "text": { "type": "string" },
                    "engine": { "type": "string" },
                    "duration_ms": { "type": "integer" },
                    "speech": { "type": "boolean" }
                },
                "required": ["text", "engine", "duration_ms", "speech"]
            },
            "annotations": { "readOnlyHint": true, "openWorldHint": false }
        },
        {
            "name": "dictate_start",
            "title": "Start dictation",
            "description": "Start recording from the microphone through the running voxtype daemon. \
                            The transcript is held for dictate_stop instead of being typed into the focused window. \
                            Fails if the daemon is not running.",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
            "annotations": { "readOnlyHint": false, "destructiveHint": false, "openWorldHint": false }
        },
        {
            "name": "dictate_stop",
            "title": "Stop dictation and return the text",
            "description": "Stop the recording started by dictate_start and wait for its transcript. \
                            status is ok (text holds the transcript), empty (no speech or too short), timeout or error.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "timeout_secs": { "type": "integer", "minimum": 1, "maximum": DICTATE_STOP_TIMEOUT_SECS, "description": "How long to wait for the transcript (default 120)." }
                },
                "additionalProperties": false
            },
            "outputSchema": {
                "type": "object",
                "properties": {
                    "status": { "type": "string", "enum": ["ok", "empty", "timeout", "error"] },
                    "text": { "type": "string" },
                    "message": { "type": ["string", "null"] },
                    "engine": { "type": "string" },
                    "model": { "type": "string" },
                    "language": { "type": ["string", "null"] },
                    "duration_ms": { "type": "integer" }
                },
                "required": ["status", "text", "engine", "model", "language", "duration_ms"]
            },
            "annotations": { "readOnlyHint": false, "destructiveHint": false, "openWorldHint": false }
        }
    ])
}

fn tool_ok(text: String, structured: Value) -> Value {
    json!({
        "content": [{ "type": "text", "text": text }],
        "structuredContent": structured,
        "isError": false
    })
}

/// A tool that ran and failed. MCP reports these as a successful JSON-RPC
/// result with `isError`, so the model sees the reason.
fn tool_err(message: String) -> Value {
    json!({ "content": [{ "type": "text", "text": message }], "isError": true })
}

fn call_tool(host: &dyn ToolHost, params: &Value) -> Result<Value, (i64, String)> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or((-32602, "tools/call needs a string \"name\"".to_string()))?;
    let args = params.get("arguments").cloned().unwrap_or(json!({}));
    match name {
        "status" => Ok(match host.status() {
            // The text block is the CLI's exact bytes; structuredContent is
            // the same object parsed, for clients that want fields.
            Ok(line) => match serde_json::from_str::<Value>(&line) {
                Ok(object) => tool_ok(line, object),
                Err(e) => tool_err(format!("status JSON did not parse: {e}")),
            },
            Err(e) => tool_err(e),
        }),
        "transcribe_file" => {
            let path = args.get("path").and_then(Value::as_str).ok_or((
                -32602,
                "transcribe_file needs a string \"path\"".to_string(),
            ))?;
            let engine = args.get("engine").and_then(Value::as_str);
            Ok(match host.transcribe_file(&PathBuf::from(path), engine) {
                Ok(t) => {
                    let structured = json!({
                        "text": t.text,
                        "engine": t.engine,
                        "duration_ms": t.duration_ms,
                        "speech": t.speech,
                    });
                    tool_ok(t.text, structured)
                }
                Err(e) => tool_err(e),
            })
        }
        "dictate_start" => Ok(match host.dictate_start() {
            Ok(message) => tool_ok(message.clone(), json!({ "message": message })),
            Err(e) => tool_err(e),
        }),
        "dictate_stop" => {
            let timeout = match args.get("timeout_secs") {
                None | Some(Value::Null) => DICTATE_STOP_TIMEOUT_SECS,
                Some(v) => v
                    .as_u64()
                    .filter(|t| (1..=DICTATE_STOP_TIMEOUT_SECS).contains(t))
                    .ok_or((
                        -32602,
                        format!(
                            "timeout_secs must be an integer from 1 to {DICTATE_STOP_TIMEOUT_SECS}"
                        ),
                    ))?,
            };
            Ok(match host.dictate_stop(timeout) {
                Ok(d) => {
                    let structured = json!({
                        "status": d.status,
                        "text": d.text,
                        "message": d.message,
                        "engine": d.engine,
                        "model": d.model,
                        "language": d.language,
                        "duration_ms": d.duration_ms,
                    });
                    let text = match d.status.as_str() {
                        "ok" => d.text,
                        other => format!(
                            "{other}{}",
                            d.message.map(|m| format!(": {m}")).unwrap_or_default()
                        ),
                    };
                    // Only a real failure is an error; `empty` is a normal
                    // outcome (the user said nothing).
                    let mut result = tool_ok(text, structured);
                    if matches!(d.status.as_str(), "error" | "timeout") {
                        result["isError"] = json!(true);
                    }
                    result
                }
                Err(e) => tool_err(e),
            })
        }
        other => Err((-32602, format!("unknown tool: {other}"))),
    }
}

/// Answer one JSON-RPC message. Returns `None` for notifications, which
/// get no reply.
pub fn handle_message(host: &dyn ToolHost, message: &Value) -> Option<Value> {
    let id = message.get("id").cloned();
    let method = message.get("method").and_then(Value::as_str);
    // A message with no id is a notification (or a stray response): never reply.
    let id = id?;
    let Some(method) = method else {
        return Some(error_reply(id, -32600, "request has no method".into()));
    };
    let params = message.get("params").cloned().unwrap_or(json!({}));
    let result = match method {
        "initialize" => Ok(json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": { "tools": { "listChanged": false } },
            "serverInfo": {
                "name": "voxtype",
                "title": "Voxtype",
                "version": env!("CARGO_PKG_VERSION")
            },
            "instructions": "Voice-to-text. `status` reports the dictation daemon; `transcribe_file` transcribes a WAV file locally."
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tool_list() })),
        "tools/call" => call_tool(host, &params),
        other => Err((-32601, format!("method not found: {other}"))),
    };
    Some(match result {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err((code, msg)) => error_reply(id, code, msg),
    })
}

fn error_reply(id: Value, code: i64, message: String) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// Serve MCP until `input` reaches EOF. Each line is one JSON-RPC message;
/// each reply is written as one line and flushed.
pub fn serve(host: &dyn ToolHost, input: impl BufRead, mut output: impl Write) -> io::Result<()> {
    for line in input.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(message) => handle_message(host, &message),
            Err(e) => Some(error_reply(
                Value::Null,
                -32700,
                format!("parse error: {e}"),
            )),
        };
        if let Some(reply) = reply {
            writeln!(output, "{reply}")?;
            output.flush()?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake;

    impl ToolHost for Fake {
        fn status(&self) -> Result<String, String> {
            Ok(r#"{"text": "idle", "class": "idle"}"#.to_string())
        }
        fn transcribe_file(&self, path: &Path, engine: Option<&str>) -> Result<Transcript, String> {
            if path.exists() {
                Ok(Transcript {
                    text: "hello".into(),
                    engine: engine.unwrap_or("whisper").into(),
                    duration_ms: 1500,
                    speech: true,
                })
            } else {
                Err(format!("cannot open {}", path.display()))
            }
        }
        fn dictate_start(&self) -> Result<String, String> {
            Ok("recording".into())
        }
        fn dictate_stop(&self, timeout_secs: u64) -> Result<Dictation, String> {
            Ok(Dictation {
                status: if timeout_secs == 1 { "timeout" } else { "ok" }.into(),
                text: "dictated".into(),
                message: None,
                engine: "whisper".into(),
                model: "base.en".into(),
                language: Some("en".into()),
                duration_ms: 2000,
            })
        }
    }

    fn run(lines: &[Value]) -> Vec<Value> {
        let input: String = lines.iter().map(|l| format!("{l}\n")).collect();
        let mut out = Vec::new();
        serve(&Fake, input.as_bytes(), &mut out).unwrap();
        String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    #[test]
    fn handshake_list_and_calls() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let replies = run(&[
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":PROTOCOL_VERSION,"capabilities":{},"clientInfo":{"name":"t","version":"0"}}}),
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
            json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"status","arguments":{}}}),
            json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"transcribe_file","arguments":{"path":file.path(),"engine":"parakeet"}}}),
        ]);
        // The notification gets no reply.
        assert_eq!(replies.len(), 4);
        assert_eq!(replies[0]["result"]["protocolVersion"], PROTOCOL_VERSION);
        let names: Vec<_> = replies[1]["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            names,
            ["status", "transcribe_file", "dictate_start", "dictate_stop"]
        );
        assert_eq!(replies[2]["result"]["structuredContent"]["text"], "idle");
        assert_eq!(replies[2]["result"]["isError"], false);
        let t = &replies[3]["result"];
        assert_eq!(t["content"][0]["text"], "hello");
        assert_eq!(t["structuredContent"]["engine"], "parakeet");
        assert_eq!(t["structuredContent"]["duration_ms"], 1500);
    }

    #[test]
    fn errors_are_reported_not_fatal() {
        let replies = run(&[
            json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"transcribe_file","arguments":{"path":"/nonexistent/x.wav"}}}),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"nope"}}),
            json!({"jsonrpc":"2.0","id":3,"method":"bogus"}),
            json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"transcribe_file","arguments":{}}}),
            json!({"jsonrpc":"2.0","id":5,"method":"ping"}),
        ]);
        assert_eq!(replies[0]["result"]["isError"], true);
        assert_eq!(replies[1]["error"]["code"], -32602);
        assert_eq!(replies[2]["error"]["code"], -32601);
        assert_eq!(replies[3]["error"]["code"], -32602);
        assert_eq!(replies[4]["result"], json!({}));
    }

    #[test]
    fn malformed_line_gets_a_parse_error_and_serving_continues() {
        let mut out = Vec::new();
        serve(
            &Fake,
            "{not json\n{\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"ping\"}\n".as_bytes(),
            &mut out,
        )
        .unwrap();
        let out = String::from_utf8(out).unwrap();
        let lines: Vec<Value> = out
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines[0]["error"]["code"], -32700);
        assert_eq!(lines[1]["id"], 7);
    }

    #[test]
    fn dictation_round_trip_and_timeout_bounds() {
        let replies = run(&[
            json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"dictate_start"}}),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"dictate_stop","arguments":{}}}),
            json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"dictate_stop","arguments":{"timeout_secs":1}}}),
            json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"dictate_stop","arguments":{"timeout_secs":0}}}),
            json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"dictate_stop","arguments":{"timeout_secs":"9"}}}),
        ]);
        assert_eq!(replies[0]["result"]["isError"], false);
        let ok = &replies[1]["result"];
        assert_eq!(ok["content"][0]["text"], "dictated");
        assert_eq!(
            ok["structuredContent"],
            json!({"status":"ok","text":"dictated","message":null,"engine":"whisper","model":"base.en","language":"en","duration_ms":2000})
        );
        assert_eq!(ok["isError"], false);
        assert_eq!(replies[2]["result"]["isError"], true);
        assert_eq!(replies[3]["error"]["code"], -32602);
        assert_eq!(replies[4]["error"]["code"], -32602);
    }
}
