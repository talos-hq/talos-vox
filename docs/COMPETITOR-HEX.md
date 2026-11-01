# Competitor report — Hex (Anomaly / OpenCode), local voice dictation

**Filed:** 2026-10-31
**Trigger:** https://x.com/opencode/status/2116555630236577819 — "no matter what agent you're
using, your voice is the best way to talk to them … and hex is the best way to use your voice:
fast, runs locally, works on mac, windows, linux. stable release now available: https://anoma.ly/hex"
**Subject:** Hex 2.3.1 (app release) / `anomalyco/hex` `main` (Cargo version 3.1.0), MIT, Rust.
Anomaly is also the company behind OpenCode.
**Purpose:** Decide whether Hex has capabilities talos-vox needs, and what (if anything) is worth
copying. Question being answered: *"do they have certain things that we need?"*

**Evidence basis:** Hex's own README, `docs/linux.md`, `docs/windows.md`, `Cargo.toml`, and the
download page (`anoma.ly/hex/`) as fetched 2026-10-31, plus talos-vox `README.md` / `Cargo.toml` /
`docs/` at commit `a45562a` (+ uncommitted `feat/kernal-api-audio` work). **Not verified:** we have
not installed or run Hex on this machine, so runtime/quality claims are Hex's own statements, not
measurements. Anything measured here is marked as such.

---

## 1. What Hex actually is

- Native Rust app (not an Electron wrapper). `cpal` capture, `transcribe-cpp` (a fork of
  whisper.cpp-family C++ engine) with Vulkan decode, plus a patched Parakeet vocabulary-rescoring
  branch (`anomalyco/transcribe.cpp`, rev `7045d1c`). Models pulled from HuggingFace at setup.
- Interaction model: **hold a shortcut, speak, release, paste at focus.** Double-tap to lock
  recording, Escape to cancel, "paste last dictation" chord. Hold *or* toggle. Tones, click-through
  HUD (Windows), recording pill (Linux/macOS), tray/menubar item.
- **Modes** — per-application (and, on macOS, per-Brave-website) rules: corrections/replacements,
  optional LLM rewrite, ordered text transformations, per-mode model/language.
- **Voice Action** — hold a second chord to rewrite selected text with a provider model.
- **Custom commands** — `~/.config/hex/hex.config.ts`, TypeScript executed in-process via
  `rquickjs` (Bun only for authoring/installing deps). Ships an agent skill
  (`npx skills add anomalyco/hex --skill hex-personal-commands`).
- **Annotate** — screen recording + on-screen ink + speech transcript, handed to a coding agent
  (macOS + Linux, not Windows). `hex annotate|show|send` from the CLI.
- **`hex mcp`** — Hex itself is an MCP server (rmcp, stdio). This is the interesting one for us:
  the dictation app exposes itself as a tool to agents rather than only being an agent accessory.
- **Local API** — a small local HTTP surface (e.g. `/profile/vitals`) and a published TypeScript SDK.
- **History** — retained pasted transcripts, 7-day default retention, entry/byte caps, "Keep: Off".
- **Distribution** — signed/notarized DMG, Authenticode (Azure Artifact Signing), Homebrew cask,
  `.deb` / `.rpm`, AUR `hex-bin`, Nix flake/nixpkgs-style guide, in-app updater with an enterprise
  kill switch (`DisableUpdates` in `anomaly.hex` / `HKLM\SOFTWARE\Policies\Anomaly\Hex`).

### Platform reality (this is where the marketing and the docs diverge)

| | macOS | Windows | Linux |
|---|---|---|---|
| Status | shipping | beta (packaged, signed later) | **beta** |
| Capture | CoreAudio | WASAPI | **ALSA only** (PipeWire via ALSA compat) |
| Decode | Metal/CPU | x64 Vulkan, ARM64 CPU-only | Vulkan, CPU fallback — **but the window still requires a working Vulkan driver** |
| Wayland | n/a | n/a | X11 + **wlroots-family only** (Hyprland, Sway, niri); GNOME/KDE paste via RemoteDesktop portal, shortcuts via GlobalShortcuts portal |
| Meetings | yes | **no** | **no** ("Meetings are macOS-only") |
| Min hardware | Apple silicon, macOS 15+ | AVX2 CPU (2013+) | **glibc 2.44+, AVX2, x86_64 or aarch64** |
| Process model | app | tray process | one process per user, systemd unit, optional window |

So the tweet's "works on mac, windows, linux" is true in the sense that binaries exist; on Linux the
product is a beta with a narrower surface than voxtype's everyday path.

---

## 2. Feature comparison against talos-vox

**Where talos-vox is clearly ahead (Hex cannot replace us):**

- **Engines.** 9 engines (Whisper, Parakeet, Moonshine, SenseVoice, Paraformer, Dolphin,
  Omnilingual, Cohere Transcribe, OpenVINO Whisper) vs Hex's single transcribe.cpp family
  (Whisper + Parakeet). Cohere Transcribe at 9–11× realtime on plain CPU has no Hex equivalent.
- **Accelerators.** MIGraphX (Radeon), CUDA 12/13, TensorRT, CoreML, Intel NPU via OpenVINO —
  Hex has Vulkan/Metal/CPU. On a Radeon or an Intel NPU box Hex is simply slower or unusable.
- **Meetings.** Voxtype has continuous chunked meeting mode with speaker attribution and
  MD/JSON/SRT/VTT export. Hex is macOS-only for meetings.
- **Desktop integration.** Compositor-native keybinds (Hyprland/Niri/Sway/River/KDE), MPRIS
  auto-pause/resume, swayosd-matched floating waveform OSD, Waybar module with `status --json`,
  Quickshell/Omarchy plugin. Hex has a tray icon and desktop portals, no documented media ducking,
  and no Waybar status module.
- **Linux robustness.** wtype → dotool → ydotool → clipboard fallback chain with CJK support; Hex
  depends on wtype-or-portal and on ALSA plus a working Vulkan driver even for CPU inference.
- **Footprint/deps.** Hex's "one process, ALSA, Vulkan" Linux story is fine on Arch, hostile on
  older enterprise/NixOS images (glibc 2.44 floor).

**Where Hex is ahead of us (candidate gaps):**

1. **Per-app / per-context Modes with LLM rewrite.** We have global `[text].replacements`, spoken
   punctuation, and one global `[output.post_process].command`. Hex routes by foreground app
   (and website) and can apply a different model, corrections, and rewrite policy per context.
2. **MCP server (`hex mcp`).** Dictation as a *tool for agents*. Nothing in voxtype exposes that.
3. **History with retention controls + "paste last".** We keep nothing; Hex gives a bounded,
   configurable recall surface plus a one-chord re-paste.
4. **Signed installers + in-app updater + enterprise disable policy.** We have signed release
   binaries from a reproducible Docker pipeline and no updater; notary/Azure signing is in progress
   in `../HANDOFF-apple-notary.md`, `../WALKTHROUGH-azure-artifact-signing.md`.
5. **In-process scripting (`hex.config.ts`, QuickJS).** We rely on shell commands in
   `post_process.command` / hooks. Cheaper for us to keep shells; theirs is nicer for users.
6. **Screen-anchored voice ("Annotate"): draw on screen, talk, hand it to an agent.** This is the
   OpenCode synergy play. We have nothing comparable — and arguably shouldn't inside a dictation
   daemon, but it sets user expectations about "voice for agents".

---

## 3. Verdict on the actual question

**"Do they have things we need?"** Yes, four, in descending order of value to us:

- **MCP surface** (strategic, small surface area to build: expose record/transcribe/status as MCP
  tools over stdio; we already have `record start|stop|toggle`, `status --json`, `transcribe`).
- **Context-aware modes** (foreground app → replacements/model/rewrite). Linux equivalent of
  "foreground app" is a real work item: needs Hyprland/Sway/KDE window lookup + a config schema,
  and is the one feature where Hex is visibly better than we are at the thing users notice.
- **History + paste-last** (cheap, high perceived value, needs a retention policy decision).
- **Updater with a kill switch** (mostly packaging work; only worth it after signing lands).

**Do we need to abandon anything for Hex?** No. Hex is not a drop-in replacement for talos-vox
on Linux: no meeting mode, no diarization, one engine family, no MPRIS/OSD/Waybar integration,
ALSA+Vulkan+glibc floors, portal-only shortcuts on GNOME/KDE. For our target users (Linux,
compositor-native, CPU-first, local-only) voxtype remains the stronger product.

**Where Hex genuinely threatens us:** the "voice for coding agents" narrative. OpenCode has
226k followers pushing one tight story (agent + voice + screen annotation + MCP) that we do not
tell at all. If an agent-native voice loop becomes the expectation, engine breadth will not save
us — the MCP surface will.

---

## 4. Recommended next steps (not yet actioned)

1. Open issues in `talos-vox` for: `hex`-style **MCP server mode**, **context-aware modes**,
   **history + paste-last**. (Report-only for now; no code written.)
2. Spike the MCP surface first — it is the smallest diff with the largest strategic payoff, and
   it makes voxtype callable from Hermes/Talos agents.
3. Keep engine and integration lead as-is; do not chase Annotate.
4. When signing lands, revisit updater + `DisableUpdates`-equivalent policy for fleet installs.

## Sources

- Tweet: https://x.com/opencode/status/2116555630236577819 (2026-10-31 15:39 UTC)
- Product page / downloads: https://anoma.ly/hex/
- Repo + README: https://github.com/anomalyco/hex (MIT) — `Cargo.toml` (version 3.1.0)
- Linux beta notes: https://github.com/anomalyco/hex/blob/main/docs/linux.md
- Windows beta notes: https://github.com/anomalyco/hex/blob/main/docs/windows.md
- vendored engine fork: https://github.com/anomalyco/transcribe.cpp
