# Integrating with Voxtype

This document is the contract for programs that drive Voxtype: desktop shells,
bar widgets, and AI agent harnesses. Anything described here is intended to stay
stable; anything not described here is an implementation detail, including the
sentinel files under `$XDG_RUNTIME_DIR/voxtype/`.

## Checking that Voxtype is available

```sh
voxtype status --format json
```

Exit code `0` means a daemon is running and reachable. The JSON is shaped for
status bars (`text`, `alt`, `class`, `tooltip`) and reports one of `idle`,
`recording`, `streaming`, `transcribing`, or `stopped` in `alt`/`class`.

## Dictating into your own program

Voxtype normally types or pastes into whatever window has focus. A program that
wants the text for itself asks for file output instead, and waits for it.

```sh
transcript="${XDG_RUNTIME_DIR:-/tmp}/myapp/dictation.txt"
rm -f "$transcript"

voxtype record start --file="$transcript" --no-auto-submit --no-smart-auto-submit

# … the user speaks; your UI shows its own indicator …

voxtype record stop --wait --json
```

`record stop --wait` blocks until the transcription is final and prints one JSON
object:

```json
{"status": "ok", "text": "the quick brown fox", "chars": 19, "message": null}
```

| `status`  | Exit | Meaning |
|-----------|------|---------|
| `ok`      | 0    | `text` holds the transcript |
| `empty`   | 3    | Nothing to transcribe: no speech detected, or the recording was too short |
| `timeout` | 4    | No outcome within `--timeout` seconds (default 120) |
| `error`   | 1    | Transcription or the file write failed; see `message` |

Without `--json`, an `ok` transcript is printed on stdout and any other outcome
is reported on stderr. The exit code is the same either way.

`--wait` needs to know which transcript to wait on. It uses the `--file` path
the recording was started with, so you normally pass nothing; use
`--wait-file <PATH>` to name it explicitly, and `--timeout <SECS>` to bound the
wait.

### Why not poll the file yourself

Because two cases have no file to poll for. If voice-activity detection finds no
speech, Voxtype deliberately transcribes nothing and writes nothing; if the write
fails, likewise. A poller cannot tell either apart from "still working", so it
waits out its own deadline and reports a timeout that never happened. `--wait`
returns `empty` or `error` in milliseconds instead.

Programs that must poll should watch for `<transcript>.done`, a JSON completion
record written after the transcript itself is complete, and only then. It is
written exactly once per file-mode recording and is consumed by `--wait`.

### Cancelling

```sh
voxtype record cancel
```

Discards the recording. No transcript and no completion record are written, so a
concurrent `--wait` ends at its timeout; cancel your own wait alongside it.

## Per-recording overrides

These apply to a single recording and are consumed by it. They are the supported
way to keep your integration from disturbing the user's own dictation settings.

| Flag | Effect |
|------|--------|
| `--file[=PATH]` | Write the transcript to a file instead of typing it |
| `--no-auto-submit` | Do not press Enter after the transcript |
| `--no-smart-auto-submit` | Ignore a spoken "submit" |
| `--profile NAME` | Apply a `[profiles.NAME]` post-processing command |
| `--model NAME` | Use a specific model for this recording |

Agent harnesses generally want the first three: the agent composes a message and
decides for itself when to send it.

A `[profiles.agent]` block with a `post_process_command` is a good place to
normalise dictation before an agent sees it, stripping filler words for instance.

## MCP server (`voxtype mcp`)

Agent hosts that speak the Model Context Protocol (Claude Code, Hermes, Talos)
can launch Voxtype as an MCP server. It is a build-time feature:

```sh
cargo build --release --features mcp
```

The server runs in the `voxtype mcp` process, talks JSON-RPC on stdin/stdout,
and exits when stdin closes. Nothing listens on a socket or port, and the
daemon is not modified. Logs go to stderr.

```json
{ "mcpServers": { "voxtype": { "command": "voxtype", "args": ["mcp"] } } }
```

| Tool | Arguments | Returns |
|------|-----------|---------|
| `status` | none | The `voxtype status --format json --extended` object, byte for byte. Needs `state_file` configured |
| `transcribe_file` | `path`, optional `engine` | `{text, engine, duration_ms, speech}`. Same pipeline as `voxtype transcribe`: replacements, spoken punctuation, VAD. `speech` is false when VAD found none |
| `dictate_start` | none | Starts a recording on the running daemon. The transcript is held for `dictate_stop` and is not typed into the focused window. Fails if the daemon is down or a dictation is already open |
| `dictate_stop` | optional `timeout_secs` (1-120, default 120) | `{status, text, message, engine, model, language, duration_ms}`. `status` uses the `record stop --wait` values: `ok`, `empty`, `timeout`, `error`. `timeout` and `error` set `isError` |

A tool that fails (missing file, model not downloaded, `state_file` unset)
returns a result with `isError: true` and the reason as text. The session
continues.

`transcribe_file` loads the model in the MCP process, not the daemon, so the
first call pays model load time.

`dictate_start` and `dictate_stop` go through the same `record start --file` /
`record stop --wait --json` contract described above. The transcript file sits
under `$XDG_RUNTIME_DIR/voxtype/mcp/` and is deleted once it has been read.
`duration_ms` is the wall time between the two calls.

## Control socket protocol

The daemon also answers on a local control socket,
`$XDG_RUNTIME_DIR/voxtype/control.sock` (a named pipe of the same name on
Windows). It is local only: a Unix socket in your runtime directory, with no
network listener. Each request is one line, and each reply is one JSON line.

Versioned requests (v1) are JSON objects. `id` is optional and is echoed back:

```text
> {"v":1,"cmd":"status","id":1}
< {"id":1,"ok":true,"state":"idle","v":1}
> {"v":2,"cmd":"status"}
< {"error":"unsupported protocol version 2; this daemon speaks v1","ok":false,"v":1}
```

v1 commands are `start`, `stop` and `status`. The daemon applies a `start` or
`stop` after it has replied, so the `state` in that reply can still show the
previous state. Send `status` afterwards to see the new one.

The original bare words `start`, `stop` and `status` are still accepted, and
always will be. Their replies stay byte-for-byte what they were
(`{"ok":true,"state":"idle"}`).

## What Voxtype will not do for you

Voxtype does not edit your configuration, and asks that you not edit its own:
`~/.config/voxtype/config.toml` belongs to the user. If you need behaviour that
is only reachable by editing it, that is a missing flag. Please open an issue.

## Note for existing integrations

Not every Voxtype has `record stop --wait`. Probe for it rather than pinning a
version, since the release lines do not carry it at the same time:

```sh
voxtype record stop --help | grep -q -- --wait
```

Older versions need the polling fallback described above.
