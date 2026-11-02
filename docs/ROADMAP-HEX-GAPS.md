# Roadmap — gaps identified against Hex

Source: [`COMPETITOR-HEX.md`](COMPETITOR-HEX.md) (2026-10-31). Each section below is written to be
lifted verbatim into a GitHub issue — this repo has issues disabled, so they live here until that
changes or they are filed upstream.

Ranked by value to us, not by Hex's prominence.

---

## 1. Expose an MCP server mode: `voxtype mcp`

## Why

Competitor Hex (Anomaly / OpenCode, MIT, Rust) ships dictation **as an MCP server** — `hex mcp` — so an agent can drive the dictation app instead of the app only serving the agent. That is the one capability in their product that has strategic value for us: it makes voxtype callable from Hermes/Talos and from any other MCP client.

See `docs/COMPETITOR-HEX.md` (§3, ranked gap #1).

## What we already have

- `voxtype record start|stop|toggle` (SIGUSR1/SIGUSR2, plus `--output-file`)
- `voxtype status --format json --extended`
- `voxtype transcribe <file>`
- existing IPC/socket layer used by the OSD and Waybar

So this is mostly a transport + schema wrapper over paths that already work; the risk is low.

## Proposal

A `voxtype mcp` stdio subcommand exposing a minimal, honest tool set:

- `dictate_start` / `dictate_stop` (returns final transcript when `--output-file` or streaming final is available)
- `transcribe_file(path)`
- `status()` → same JSON as `status --format json --extended`
- `meeting_start` / `meeting_stop` (optional, second pass — export paths, not transcripts)

Design notes:

- stdio transport only to start; no listening socket, no auth surface. Anything that wants a socket can wrap it.
- Must not require the daemon to be running in a special mode — reuse the existing control path.
- Return transcripts as text plus a structured block (`engine`, `model`, `duration_ms`, `language`) so agents can log provenance.
- Keep it out of the default `cargo build` path if the MCP dependency is heavy; feature-gate if needed.

## Acceptance

- `voxtype mcp` speaks MCP over stdio; a smoke test drives one dictation round-trip end-to-end.
- Documented in `docs/INTEGRATIONS.md` with a copy-paste Hermes/Claude-style client config.
- No behaviour change when the subcommand is not used.

## Why

Voxtype's text handling is global: one `[text].replacements` table, one `spoken_punctuation` flag, one `[output.post_process].command`, one active engine/model. Hex (Anomaly / OpenCode) routes by **foreground application** (and by website on macOS): per-app corrections, per-app model/language, per-app LLM rewrite, per-app transformations.

This is the gap users feel first — dictating code into an editor, prose into a mail client, and shell into a terminal want different engines and different cleanup. See `docs/COMPETITOR-HEX.md` (§3, ranked gap #2).

## Current state (verified by grep, not assumption)

- `src/config/root.rs` — single global `[text]` / `[output]` config; no app-scoped table.
- `src/output/post_process.rs` — `process_with_context` passes `VOXTYPE_CONTEXT` (finalized text so far), no window/app identity.
- No `src/` module resolves the focused window (no Hyprland/Sway/KDE client lookup).

## Proposal

Stage it so stage 1 is cheap and useful on its own:

1. **Class resolution.** Add a `focused_app()` helper with backends:
   - Hyprland: `hyprctl activewindow -j` (`class`, `title`)
   - Sway: `swaymsg -t get_tree -r` walk to focused node (`app_id`, `name`)
   - KDE/GNOME: `_NET_ACTIVE_WINDOW` via X11 where available, else `None`
   - Fallback: `None` (behaviour identical to today)
2. **Config schema.** `[[modes]]` array: `match = { app = ["code", "Cursor"], title_regex = "..." }` with optional `engine`, `model`, `replacements`, `spoken_punctuation`, `post_process.command`.
3. **Plumbing.** Resolve once per recording (not per chunk), pass to both the engine selection path and post-processing; export as `VOXTYPE_APP` / `VOXTYPE_APP_TITLE` alongside `VOXTYPE_CONTEXT` so shell hooks can branch today, before the config schema exists.
4. **First-match-wins**, with `voxtype config` printing which mode matched.

## Notes / risks

- Wayland focus lookup is racy and compositor-specific; a `None` result must degrade to global config silently (never block dictation).
- Do **not** put model swaps on a fast path — model load is seconds; if a mode changes the engine, load lazily and keep the previous model warm if memory allows.
- Stage 3 (env vars only) is a small independent PR and worth doing even if the rest slips.

## Acceptance

- `[[modes]]` documented in `docs/CONFIGURATION.md` with at least one worked example (editor vs chat).
- On Hyprland: dictating into two different apps applies different replacements; verified by hand.
- With no matching mode or no compositor support: byte-identical behaviour to current `dev`.

## Why

Voxtype discards every transcript after typing it. There is no recall surface and no "paste that again" chord. Hex (Anomaly / OpenCode) keeps pasted transcripts with explicit retention controls (7-day default, entry and byte caps, "Keep: Off", "Clear all") and a paste-last shortcut.

For us the immediate value is smaller than MCP, but it is cheap and it shows up in every-day use: a mis-paste, a target app that ate the text, a dictation you want to re-send into a different window. See `docs/COMPETITOR-HEX.md` (§3, ranked gap #3).

## Proposal

- **Opt-in storage.** `[history] enabled = false` by default (privacy-first, no audio ever).
  - `max_entries`, `max_bytes`, `retention_days` (defaults: 500 / 2 MiB / 7 days), `path` (`$XDG_STATE_HOME/voxtype/history.jsonl` by default).
  - Entry: timestamp, final text, engine, model, duration_ms, app/title if context-aware modes land, **never** audio.
- **CLI.** `voxtype history list [--limit N] [--json]`, `voxtype history clear`, `voxtype history last`.
- **Paste-last.** `voxtype paste-last` re-types/re-pastes the newest entry through the existing output chain (wtype → dotool → ydotool → clipboard), plus an optional bind example in the README.
- **Redaction.** History must respect the existing config redaction rules for anything printed to logs; `--json` output is the only machine surface.

## Acceptance

- History is off by default and nothing is written when disabled.
- Caps and retention are enforced on write (bounded file, not unbounded append).
- `voxtype history clear` removes the file and the in-memory copy.
- `voxtype paste-last` works on Hyprland/Sway with both wtype and portal-less fallback verified by hand.
- Unit tests cover cap enforcement, retention expiry, and disabled-mode no-op.

