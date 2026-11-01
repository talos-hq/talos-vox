# Absorbing Hex into talos-vox — decision plan

Date: 2026-11-01. Repo: `/home/niteris/dev/talos-hq/talos-vox` @ `8d63107` (branch `feat/kernal-api-audio`, voxtype 1.1.1). Hex: shallow clone of `github.com/anomalyco/hex` main into `/tmp/hexsrc` (succeeded). Analysis only; nothing in the repo was modified.

**Headline correction to the brief:** Hex is no longer "2.3.1". Its `Cargo.toml` says `version = "3.1.2"`, and its desktop UI is **not Qt**. It is Anomaly's own Rust/wgpu toolkit `rune` (`rune = { git = "https://github.com/anomalyco/rune.git", ... features = ["native-file-dialog"] }`, plus `wgpu = { version = "30", ... "metal" ... }`). The competitor that sets the bar for cross-platform dictation shipped it **without Qt**. That fact shapes the UI decision in §3.

---

## 1. Current architecture, as it is

| Layer | Files | Notes |
|---|---|---|
| Entry / CLI | `src/main.rs`, `src/cli/*.rs` (clap), `src/app/dispatch.rs` | `src/app/macos.rs` and `src/app/updates.rs` exist already |
| Daemon core | `src/daemon.rs` (6,181 lines), `src/state.rs`, `src/eager.rs` | One `tokio::select!` loop. Unix signals at `src/daemon.rs:37`, `:367`, `:408`, `:6116`, `:6134` (`#[cfg(unix)]`) |
| Control / IPC | `src/control.rs` (144 lines), `src/local_ipc.rs` (72), `src/daemon_status.rs` | Line protocol `start`/`stop`/`status` → one JSON line (`src/control.rs:1-9`). Socket at `runtime_dir()/control.sock` (`src/control.rs:29`). Unix uses `UnixStream`. Other hosts use kernal-api named pipes (`src/local_ipc.rs:1-5`) |
| Status contract | `src/status_json.rs` | Waybar JSON, locked byte-for-byte by tests (`src/status_json.rs:21-22`, `:135-146`) |
| Hotkey | `src/hotkey/evdev_listener.rs` (Linux, `src/lib.rs:81`), `src/hotkey_macos.rs` (rdev, Accessibility, `src/hotkey_macos.rs:1-7`, `src/lib.rs:83`) | Nothing on Windows |
| Audio | `src/audio/cpal_capture.rs`, `src/audio/kernal_capture.rs` (feature `kernal-api`), `dual_capture.rs` (meeting loopback), `media.rs` (MPRIS) | cpal 0.18 |
| Engines | `src/transcribe/*` — whisper, parakeet(+streaming), moonshine, sensevoice, paraformer, dolphin, omnilingual, cohere, openvino_whisper, remote, soniox, cli, subprocess worker | Enum at `src/config/engines/mod.rs:42` |
| Output chain | `src/output/mod.rs` + wtype/dotool/ydotool/eitype/xclip/clipboard/paste (Linux); cgevent/osascript/pbcopy (macOS, `src/output/mod.rs:22,32,35`); `desktop_input.rs` (kernal-api, non-Linux, `:26`) | `driver_order` is "not yet supported on macOS" (`src/output/mod.rs:339`) |
| Text | `src/text/mod.rs`, `src/output/post_process.rs` | Post-process gets `VOXTYPE_CONTEXT` env (`post_process.rs:45,98`), with no app identity |
| OSD | `src/osd/*` (IPC, supervisor, theme), bins `voxtype-osd-native` (Wayland layer-shell + wgpu + egui, `Cargo.toml` feature `osd-native`), `voxtype-osd-gtk4`, `voxtype-osd-quickshell` | All three renderers are Wayland/Linux |
| Menubar / tray | `src/menubar.rs` (macOS only, `src/lib.rs:87`; `tray_icon::TrayIconBuilder` at `:519`), deps `tao 0.32`, `tray-icon 0.21.3` | macOS tray already exists in Rust |
| TUI | `src/tui/*` (~14k lines, ratatui). Includes `compositor_bindings.rs` (1,272) and `waybar_section.rs` | Linux-flavoured sections |
| Meeting | `src/meeting/*` (rusqlite bundled, diarization, export, summary) | SQLite already in tree |
| Setup | `src/setup/{systemd,waybar,compositor,quickshell,dms,hammerspoon,launchd,app_bundle,macos}.rs` | |
| Notifications | `src/notification.rs`, `src/daemon.rs:216-245` | Linux: notify-send. macOS: terminal-notifier/mac-notification-sys. Windows: none (`:243-245`) |

**Linux-only, and why:**
- evdev: kernel input devices plus the `input` group.
- wtype / dotool / ydotool: Wayland virtual-keyboard and uinput.
- MPRIS (`src/audio/media.rs:37-39`): D-Bus.
- All three OSDs: Wayland layer-shell, or a quickshell QML plugin.
- Waybar and compositor keybind setup: wlroots/Hyprland ecosystem.
- `nix`/`inotify`/`zbus` in `[target.'cfg(target_os = "linux")']`.

**Already cross-platform:** the macOS slice, the kernal-api Windows control/audio/desktop-input work on this branch (`cd0ec20`, `7fd45fe`), and `local_ipc` named pipes.

## 2. Target architecture: pick (a), daemon-as-lib + IPC, with thin UI clients

**Decision: (a).** `voxtype` stays a headless daemon, and the `voxtype_lib` crate stays the product. The desktop app (`voxtype-app`) is a separate binary and a client of the control channel. On macOS and Windows the app *launches and supervises* the daemon: a login item or launch agent, the same pattern as `src/osd/supervisor.rs`. To the user it looks like one app.

Reasons:
1. Every existing contract is already a client of a socket or the CLI: Waybar polls `status --json` (`status_json.rs`), compositor keybinds call `voxtype record toggle`, the OSD reads `osd/ipc.rs`, and the TUI edits config. Option (b) would turn each of these into a regression risk.
2. GPU isolation (`src/transcribe/subprocess.rs`) and ggml-vs-ggml address-space separation (CLAUDE.md, 1.3.0) already assume a process boundary.
3. The MCP server, CLI, Waybar and app become peers on one protocol, so there is one code path to test.

What changes about the boundary: `control.rs` grows from 3 verbs into a versioned JSON-lines protocol (`{"v":1,"cmd":"status"}`). The bare legacy words `start`/`stop`/`status` stay accepted forever. Add `subscribe` (state stream), `history.*`, `paste_last`, `config.reload`, `context`. Today OSD levels go over a separate socket. Keep it separate; it is a high-rate stream.

Contracts under (a):
- `status --json`: unchanged, still byte-locked.
- OSD: Linux keeps `voxtype-osd-*`. macOS and Windows get a HUD inside `voxtype-app` that speaks the same `osd/ipc.rs` protocol.
- Waybar and the compositor keybind contract: untouched.
- TUI: stays the Linux/headless config editor. Freeze it at feature parity, and add new settings via `config_schema.rs` so both front-ends render from one schema (see §10, "two UI stacks").

**What would change my mind:** macOS refusing to let a helper daemon hold the Accessibility/Input Monitoring grant on the app's behalf. TCC attributes permissions to the responsible process; a child inside the same signed `.app` bundle normally inherits it. UNVERIFIED — prove it by notarizing a test bundle where `Contents/MacOS/voxtype-app` spawns `Contents/MacOS/voxtype daemon` and checking whether CGEventTap works after one grant. If it fails, fall back to (c): on macOS the app links `voxtype_lib` and runs the daemon loop in-process, keeping the socket for CLI/MCP. Confidence in (a): about 75%.

## 3. UI toolkit decision: **not Qt. Use egui/eframe + `tray-icon`. Runner-up: Qt 6 via cxx-qt, dynamically linked.**

**Challenging "migrate it into rust".** The reading "Qt glue must be Rust" is right, but cxx-qt does not get you there. cxx-qt lets you "implement QObject subclasses in Rust which can be used from C++, QML, and JavaScript" (KDAB/cxx-qt README). The UI itself is still written in QML, a declarative JS dialect with its own runtime. So "Qt, all-Rust" is not attainable. With cxx-qt the team maintains Rust plus QML plus a C++ build (CMake or cxx-qt-build) plus a Qt deploy step. That is the dialect the user said they did not want.

Why egui wins for this product:
- **Scope.** About 3 windows (settings, history, model picker) plus a HUD and a tray. Qt's widget depth is not needed.
- **Already in tree.** egui 0.34 + egui-wgpu + wgpu 29 power `voxtype-osd-native` (`src/bin/voxtype_osd_native/app.rs:1`). `tao` + `tray-icon` power the macOS menubar (`src/menubar.rs:14-16,519`). Most of the work is generalizing existing code, not adopting a new stack.
- **Licensing.** MIT/Apache, no obligations. Qt LGPLv3 requires shipping or offering Qt's "complete corresponding source code", letting "the user ... change and re-link the library", and notes static linking means the app "may no longer be 'work that uses the library'" (qt.io LGPL obligations page). In practice that means dynamic Qt frameworks inside the `.app` and DLLs beside the `.exe`. Each of those must be signed and notarized (each nested Mach-O needs a hardened-runtime signature), and `macdeployqt`/`windeployqt` add a dedicated pipeline stage. Expect roughly +40–80 MB per platform. UNVERIFIED — measure with `macdeployqt` on a hello-world QML app.
- **Hex's precedent.** A pure-Rust GPU UI (rune/wgpu) ships on macOS, Windows and Linux at 3.1.2.
- **Single static Rust binary.** Keeps Homebrew cask, WinGet and AUR trivial, and keeps the Docker reproducibility pipeline (CLAUDE.md "Building Release Binaries") intact. No Qt SDK is needed in the Ubuntu 22.04 containers.

Where egui is weaker:
- Native look: acceptable for a settings app.
- Accessibility: egui has AccessKit integration. UNVERIFIED — check screen-reader output on macOS.
- Click-through transparent HUD on Windows and macOS needs window-level flags beyond eframe, via `raw-window-handle` + objc2/windows-sys.

Tray: `tray-icon` (tauri-apps) covers macOS NSStatusItem, Windows Shell_NotifyIcon and Linux (libappindicator/ksni). On Linux the existing Waybar module stays the primary path.

- **Runner-up: cxx-qt + Qt 6 LGPL (dynamic).** It wins if the product needs rich accessible native widgets, or if the team already has QML designers.
- **Third: Slint.** Its royalty-free licence "Permits use in proprietary desktop ... applications at no cost" "as long as you disclose that you use Slint" (slint LICENSE.md). It still means a `.slint` DSL.
- **iced** is viable, but would add a second immediate-mode/Elm-style stack beside egui.

Confidence: about 70%. **What would change my mind:** a hard requirement for native widget accessibility (VoiceOver/Narrator) that egui/AccessKit fails in a one-day spike.

## 4. Absorbing Hex, feature by feature

### 4.1 Modes (context-aware rules) — IN, milestone M3
Hex: `src/dictation/modes.rs`, plus per-OS context modules:
- `src/windows/context.rs`: "the focused application's executable stem, and for known browsers the page URL read through UI Automation", with a `URL_READ_DEADLINE` of 300 ms on a helper thread.
- `src/linux/context.rs`: "X11 reports the active window's process and title. On Wayland the compositor's foreign-toplevel list (wlroots compositors, niri, Hyprland) ... desktops without it (GNOME, KDE) report no focused window, and modes fall back to the global settings."

Sketch for this repo:
- New `src/context/mod.rs` with `trait FocusProbe { fn snapshot(&self) -> Option<AppContext> }`, where `AppContext { app_id, exe, title, url: Option }`.
- Implementations:
  - `macos.rs`: `NSWorkspace.sharedWorkspace.frontmostApplication.bundleIdentifier` via `objc2-app-kit` (Hex already depends on `objc2-app-kit 0.3.2`).
  - `windows.rs`: `GetForegroundWindow` → `GetWindowThreadProcessId` → `QueryFullProcessImageNameW` (windows-sys).
  - `linux_wlr.rs`: `zwlr_foreign_toplevel_manager_v1`; `wayland-protocols-wlr` is already an optional dep.
  - `linux_hypr.rs`: `hyprctl activewindow -j`.
  - `linux_sway.rs`: `swaymsg -t get_tree`.
  - `x11.rs`.
  - KDE/GNOME: none; fall back to global settings, same as Hex.
- Take the snapshot **at record start** (`daemon.rs` record path), not at output time, so a focus change during transcription does not misroute.
- Config: `[[modes]]` array in `src/config/profile.rs` (profiles already exist; this aligns with upstream #519 "unified profiles"):
  ```toml
  [[modes]]
  match = { app = ["code", "com.microsoft.VSCode"], url_host = [] }
  profile = "code"        # engine/model/language/replacements/post_process
  ```
  Resolution: first match wins; otherwise the default profile.
- Pass `VOXTYPE_APP_ID`/`VOXTYPE_APP_TITLE` into `post_process` next to `VOXTYPE_CONTEXT` (`post_process.rs:98`).
- Engine switching per mode is costly when models differ. Limit it to engines already loaded, or ones that `prepare()` can hide behind recording time.

### 4.2 MCP server `voxtype mcp` — IN, first PR
Hex builds it with `rmcp = { version = "3.5.1", default-features = false, features = ["server","macros","transport-io"] }`, using `#[tool_router]` / `#[tool(...)]` (`/tmp/hexsrc/src/meeting/mcp.rs:180-298`). Hex's MCP serves **meetings** (list/search/notes), not live dictation.

Ours:
- New `src/mcp.rs` behind feature `mcp` (in the default build once it is small enough), stdio only.
- Tools:
  - `status` → the `status --json --extended` payload.
  - `dictate_start`, `dictate_stop` → returns `{text, engine, model, duration_ms, language}`. Needs the new control verb `stop_and_return`, which today only exists via `--output-file`.
  - `transcribe_file(path)` → reuses `src/app/transcribe_file.rs`.
  - `history_list` / `paste_last` once M2 lands.
  - `meeting_*` second.
- It lives in the **CLI process** as a control-channel client, not in the daemon or the UI. Every OS gets it for free, with no new listening surface.

### 4.3 History + paste-last — IN (opt-in, default off), M2
Hex: `src/history.rs` with `HistoryRetention` (Week = "7 days", Month = "30 days", `:29-49`). Hex's Windows docs list "Paste Last Dictation" on `Ctrl+Alt+V`.

Ours:
- `src/history/mod.rs` on the already-bundled `rusqlite` (`Cargo.toml` dep, used by meeting).
- Path via `directories::ProjectDirs::data_local_dir()`:
  - Linux: `$XDG_STATE_HOME/voxtype/history.db` (state, not data, since it is regenerable).
  - macOS: `~/Library/Application Support/voxtype/`.
  - Windows: `%LOCALAPPDATA%\voxtype\`.
- `[history] enabled=false, retention_days=7, max_entries=500, store_audio=false`. Fold upstream #529 `retain_audio` in here.
- Redaction: apply replacements first, then optional regex `redact = [...]`. Never store when the mode is marked `private = true` (password managers, terminals by default).
- `voxtype paste-last` = new control verb → re-run the output chain. Pruning runs on daemon start and hourly.

### 4.4 Custom command scripting (QuickJS + TS) — SKIP, keep shell hooks
Hex embeds `rquickjs 0.14` plus the `oxc_*` parser stack to strip TS types (`src/personal_commands/script.rs:1-25`; 64 MB memory limit, 2 s load limit). That is 6+ crates of maintenance and a sandbox to defend. Voxtype already has `[output.post_process].command` with stdin/stdout and env context, plus pre/post hooks (commit `97f2b47`). These are more general for Linux power users and work on every OS.

Revisit only if Windows users demonstrably cannot write hooks. Then prefer exposing hooks as PowerShell-friendly before embedding a JS runtime.

### 4.5 Updater — DEFER until signing exists, then hand-roll the minimal version
Hex hand-rolls everything (`src/updater/{macos,windows,linux}.rs`):
- macOS: verifies Developer ID plus notarization, then atomic `renamex_np(RENAME_SWAP)`.
- Windows: "the download must carry a valid Authenticode signature".
- Linux: ed25519 (`ed25519-dalek`).

An updater before notarization or Authenticode can only be trust-on-first-use, so it is pointless. Plan:
1. M5 ships signed artifacts via Homebrew cask and WinGet, which update themselves. Keep `src/app/updates.rs` as a check-and-notify.
2. M7 (optional): Sparkle 2 on macOS (ObjC framework, EdDSA appcast; embedding needs signing of the nested framework) versus Hex-style self-swap. Prefer the Hex-style hand-roll on Windows and macOS, because adding Sparkle and WinSparkle means two foreign frameworks in an otherwise all-Rust build. Linux: never self-update packaged installs; AppImage only, with an ed25519 signature.

### 4.6 Annotate (screen draw + voice → agent) — OUT
Hex compiles a separate capture engine (`scrawl-capture`, `screencapturekit = "=11.0.0"`, plus a `crates/capture` workspace member) that feeds OpenCode. Without our own agent surface it is a screenshot tool.

Revisit once the Talos/Hermes MCP integration exists. Then the right shape is an MCP tool `capture_region` returning an image, not a drawing UI.

## 5. Platform abstraction plan

| Concern | Linux (today) | macOS | Windows | Crates (maintained, verified in a Cargo.toml) | Permission prompt |
|---|---|---|---|---|---|
| Audio | cpal/PipeWire; kernal-api capture | CoreAudio via cpal | WASAPI via cpal | `cpal 0.18` (both repos) | macOS Microphone (TCC; needs `NSMicrophoneUsageDescription` + `com.apple.security.device.audio-input` under hardened runtime). Windows 10/11 Privacy › Microphone toggle, no prompt for desktop apps. Loopback for meetings: Hex uses `objc2-core-audio` taps on macOS and WASAPI loopback on Windows |
| Global hotkey | evdev; compositor binds | rdev today (`hotkey_macos.rs`); move to CGEventTap via objc2/core-graphics | `WH_KEYBOARD_LL` or `RegisterHotKey` (windows-sys) | `rdev 0.5.3` (ours), `windows-sys 0.61` (Hex). `global-hotkey` (tauri) UNVERIFIED for push-to-talk key-up semantics | macOS Input Monitoring (event tap listen) or Accessibility. Windows: none. Linux Wayland: GlobalShortcuts portal (Hex uses it via `zbus`) as a third path beside evdev |
| Text injection | wtype/dotool/ydotool/eitype | CGEvent Unicode (`output/cgevent.rs`) | `SendInput` with `KEYEVENTF_UNICODE`; via kernal-api `desktop-input` today | `core-graphics 0.24`, kernal-api, windows-sys | macOS Accessibility (CGEventPost to other apps). Windows: UIPI blocks injection into elevated windows, so document it |
| Clipboard + paste | wl-copy/xclip | pbcopy → `arboard` | `arboard` | `arboard 3.6.1` (Hex, mac + win) | none |
| Tray | Waybar (keep) + optional tray-icon | tray-icon (exists) | tray-icon | `tray-icon 0.21.3` | none |
| App context | §4.1 | NSWorkspace | GetForegroundWindow | `objc2-app-kit 0.3.2`, windows-sys | macOS: frontmost bundle ID needs no prompt; window *titles* need Screen Recording. Avoid titles on macOS |
| Notifications | notify-send | mac-notification-sys | none today (`daemon.rs:243`). Needs a toast; crate UNVERIFIED | | macOS notification prompt |
| Login item | systemd user unit | launchd (`setup/launchd.rs`) or `SMAppService` (`objc2-service-management`, Hex) | HKCU Run key (`winreg 0.55`, Hex) | | macOS "Login Items" approval |

Rule: each concern gets a trait in `src/platform/<concern>/mod.rs` with `#[cfg]` impl files. That replaces today's scattered `#[cfg(target_os)]` in `daemon.rs`/`output/mod.rs` (about 25 sites, §1).

## 6. Engines across platforms

| Engine | macOS arm64 | Windows x64 | Notes |
|---|---|---|---|
| Whisper (whisper-rs 0.16) | Yes, `gpu-metal` | Yes, CPU / `gpu-vulkan` / `gpu-cuda` | Ship as the default everywhere |
| Cohere GGUF (#792, transcribe.cpp) | Yes (Metal) | Yes (Vulkan) | Hex pins `transcribe-cpp = "=0.4.1"` + `vulkan` on win-x64. Strategic per CLAUDE.md |
| Parakeet (parakeet-rs/ORT) | `parakeet-coreml` exists | CUDA/DirectML. DirectML feature UNVERIFIED in parakeet-rs 0.3.5 | Move to parakeet.cpp (#483) |
| Moonshine/SenseVoice/Paraformer/Dolphin/Omnilingual (ORT) | CPU only | CPU / CUDA | Feature-gate off desktop builds. They die at 2.0 anyway |
| OpenVINO (`openvino-genai 0.11`, runtime-linking) | No | Possible (Intel NPU) | Windows opt-in later only |
| Remote / Soniox / CLI | Yes | Yes | Pure HTTP/WS |

- Desktop builds ship **Whisper + Cohere-GGUF + remote** only. That avoids the ORT prebuilt (AVX-512 leakage, about 20–40 MB; UNVERIFIED size, measure with `ls -l` on onnx-avx2 versus avx2 release artifacts).
- macOS: arm64 only at first, like Hex ("macOS (Apple silicon)" is its only DMG). Universal doubles the size for little audience.
- Model download UX: the existing `src/model_manager.rs`/R2 CDN with sha256 (`sha2` dep). The app shows a first-run picker. Models live in the platform data dir, never inside the signed bundle, so updates and notarization never touch model bytes.

## 7. Signing and distribution

**macOS**
- `.app` bundle: `src/setup/app_bundle.rs` and `scripts/build-macos-dmg.sh` already exist. Today that script ad-hoc signs (`:127`, `codesign --force --sign -`), and so does the Homebrew formula (`packaging/homebrew/voxtype.rb:91`).
- `build-macos.yml:93-132` already imports a Developer ID into a keychain.
- Add:
  - `--options runtime` and `--timestamp`.
  - Entitlements: `com.apple.security.device.audio-input`. Add `com.apple.security.cs.disable-library-validation` only if ORT or Qt dylibs are loaded. Avoid it, which is another reason to skip Qt and ORT.
  - Info.plist `NSMicrophoneUsageDescription`.
  - Accessibility and Input Monitoring have no entitlement. They are user TCC grants, and notarization does not review them (UNVERIFIED that the notary never flags `AXIsProcessTrusted` usage; a test submission proves it).
  - `notarytool submit --wait` → `stapler staple`. Credentials per `HANDOFF-signing-credentials.md` / keyz RUNBOOK (team `NCFFSNX26D` is the *talos* repo's; talos-vox needs its own secrets set).
  - Sign the daemon binary *and* the app binary inside the bundle with the same identity, so TCC sees one responsible app (§2 risk).
- Homebrew: a cask in a `talos-hq/tap`, like Hex's `brew install --cask anomalyco/tap/hex`.

**Windows**
- Mirror Hex `docs/windows-signing.md`. Build jobs hold no secrets; a separate `sign-windows` job in a `release` environment runs `azure/login` (OIDC) and `artifact-signing-action` (SHA-256, RFC 3161 timestamp), then verifies.
- Then an installer (Inno Setup per Hex, per-user, no admin), signed again.
- Follow `WALKTHROUGH-azure-artifact-signing.md`.
- WinGet manifest pointing at the signed installer.
- Note the user rule: no new workflow files unless requested. Adding signing jobs means editing the existing `build-*.yml`, or asking first.

**Linux**
- Unchanged: Docker builds, deb/rpm (`scripts/package.sh`), AUR.
- Add a detached signature on `SHA256SUMS.txt` (already CI-signed per CLAUDE.md).
- AppImage optional.

**Qt's effect, if it were chosen:**
- Every Qt framework and plugin re-signed inside the bundle.
- `windeployqt` output (about 30+ DLLs) each Authenticode-signed.
- An LGPL source offer per release.
- Qt SDK in each Docker image, which breaks the "Ubuntu 22.04 caps glibc 2.35" story unless Qt is self-built.

Egui avoids all four.

Reproducibility: Linux stays Docker. macOS and Windows cannot be Docker-built; pin toolchains via `soldr`, pin runner images, and publish SHA256SUMS from the unsigned build job so the signing job is auditable (Hex's "verify the artifacts' digests" step).

## 8. Divergence strategy

Measured state:
- `upstream` remote exists. `git rev-list --left-right --count upstream/dev...origin/dev` = `0 0`, so the fork's `dev` is identical to upstream today.
- All divergence is on this branch: `git log origin/dev..HEAD` = **8 commits**, `36 files changed, 2890 insertions(+), 1328 deletions(-)`.
- The divergence is the kernal-api dependency (`Cargo.toml`: `kernal-api = { git = "https://github.com/forge-ci/kernal-api", branch = "main", ..., optional = true }`). It is a git-branch dep, which is not upstreamable as is.

The merge burden is small *now*. It grows mainly through `daemon.rs` (6,181 lines; every feature touches it) and `output/mod.rs`.

**Upstream (small, general):**
- `voxtype mcp` (feature-gated).
- History + paste-last.
- `VOXTYPE_APP_ID` context and the Linux focus probes (Hyprland/Sway/wlr).
- The versioned control protocol.
- Windows `notification` stub.
- Config `[[modes]]`, presented as the realization of upstream #519.

These match the upstream roadmap (1.3.0 profiles, 1.5.0 history #209), so they have a real chance of review.

**Fork-only:**
- `voxtype-app` (egui desktop app).
- macOS/Windows signing and installers.
- kernal-api integration.
- Windows hotkey/injection.
- Updater.

**Mechanics that keep merges cheap:**
1. New code goes in new modules (`src/context/`, `src/history/`, `src/mcp.rs`, `src/platform/`, `crates/voxtype-app/`). Touch `daemon.rs` only through 1–5 line hook calls.
2. Every fork-only behavior sits behind a cargo feature (`kernal-api`, `desktop-app`, `win-input`) that defaults off in upstream's build.
3. Make the repo a workspace (the `[workspace] members = ["xtask"]` already exists) and add `crates/voxtype-app`, so UI code never conflicts.
4. Rebase or merge `upstream/dev` weekly.

**Clean break justified when:** upstream rejects the control-protocol and MCP PRs, *or* the 2.0 ONNX removal lands with a daemon restructure that conflicts with `src/platform/`, *or* weekly merges cost more than about 2 h for 3 consecutive weeks. Then rename the crate, keep `voxtype` CLI compatibility, and cherry-pick engines by hand.

## 9. Sequenced plan (one PR per feature; stacked when same files)

**This week, no UI toolkit needed:**
- PR1 `voxtype mcp` (stdio, `status` + `transcribe_file` only; rmcp feature-gated). Acceptance: an MCP client lists and calls both tools in an integration test. No change without the subcommand.
- PR2 Control protocol v1 (JSON lines, legacy words kept), with a test that the legacy `start` still replies the same. Extends `tests/control_channel.rs`.
- PR3 (stacked on PR2) `stop_and_return` verb, plus MCP `dictate_start/stop`.
- PR4 History store (opt-in) + `voxtype history list` + `voxtype paste-last`. Acceptance: off by default; retention prune test; data lands in the per-OS dir.
- PR5 `VOXTYPE_APP_ID` via the Hyprland/Sway probe into post_process.

**Later milestones:**
- **M1, finish this branch** (kernal-api Windows/macOS). Acceptance: CI builds the lib on Windows and macOS. Merge before anything else touches `output/mod.rs`.
- **M2, History/paste-last.** PR4 plus a hotkey binding. MCP `history_list`.
- **M3, Modes.**
  - PR a: `src/context/` trait + Linux probes.
  - PR b: macOS NSWorkspace probe.
  - PR c: Windows foreground probe.
  - PR d: `[[modes]]` config + profile resolution at record start.
  - Acceptance: the same dictation in two apps yields different replacements; on GNOME/KDE it falls back cleanly.
- **M4, Windows input parity.** Hotkey (WH_KEYBOARD_LL push-to-talk), SendInput Unicode, arboard paste, toast notification, Run-key login item. One PR each.
- **M5, Signing.**
  - PR a: macOS hardened runtime + entitlements + notarize + staple in `build-macos.yml`.
  - PR b: Windows Azure Artifact Signing + Inno installer.
  - PR c: Homebrew cask + WinGet manifest.
  - Acceptance: `spctl -a -vv` reports "Notarized Developer ID"; `Get-AuthenticodeSignature` reports Valid on a clean VM.
- **M6, `voxtype-app`** (egui).
  - PR a: tray + daemon supervisor (generalize `menubar.rs`).
  - PR b: settings rendered from `config_schema.rs`.
  - PR c: history window.
  - PR d: model picker.
  - PR e: HUD on macOS/Windows over the `osd/ipc.rs` protocol.
  - Acceptance: first-run on a clean Mac/Win VM leads to dictation working with only the OS prompts.
- **M7 (optional), signed self-updater.**

## 10. Risk register

| Risk | P | Blast radius | Cheapest falsifying test |
|---|---|---|---|
| TCC grants don't carry from app to helper daemon | Med | Forces architecture (c) | Notarized 2-binary bundle spike, CGEventTap after one grant (1 day) |
| Notarization rejects or flags accessibility / event-tap use | Low–Med | Blocks macOS release | Submit the current `voxtype` binary with hardened runtime now; read the notary log |
| Qt licence/deploy surprise (if Qt is chosen anyway) | High | Bundle size, LGPL source duty, signing every framework | macdeployqt hello-world: measure size and notarize it |
| egui can't do click-through HUD / accessibility on Win/mac | Med | HUD falls back to native window code | 1-day spike: transparent always-on-top click-through eframe window on both OSes |
| Wayland focus lookup unreliable (GNOME/KDE none; Hyprland/Sway IPC only) | High (known) | Modes silently global on those DEs | `voxtype context --watch` debug cmd on GNOME, KDE, Hyprland, Sway |
| Upstream divergence cost grows via `daemon.rs` | Med | Weekly merge pain | Track merge time per week; trip the §8 break rule |
| Binary/model size (ORT, CUDA) on desktop builds | Med | Download/notarization time | Compare current release artifact sizes; build desktop feature set and `ls -l` |
| Two UI stacks (TUI + OSD ×3 + egui app) drift | High | Settings exist in one UI only | CI test that every `config_schema.rs` key is rendered by both TUI and app |
| Windows UIPI blocks injection into elevated apps | High (known) | Dictation fails in admin terminals | Manual test into an elevated PowerShell; document the limitation |
| Windows SmartScreen reputation for a new signer | High | Scary first-run dialog | Unavoidable early; Hex docs warn the same. Sign early to accrue reputation |
| kernal-api git-branch dep breaks builds | Med | All non-Linux builds | Pin to a rev in Cargo.toml now |

## 11. Open questions (only design-changing)

1. **Is "Qt" a requirement or a proxy for "one cross-platform UI"?** Default: proxy, so egui. If it is a requirement, cxx-qt + dynamic Qt 6 LGPL, and accept QML.
2. **Do we still want upstream acceptance, or fork-first?** Default: upstream-first for the Qt-independent features (§8 list), fork-only for app and signing.
3. **macOS universal or arm64-only?** Default: arm64-only, like Hex.
4. **Signing identity:** is talos-vox signed under the same Apple team (`NCFFSNX26D`) and Azure account as talos? Default: yes, with separate repo secrets.
5. **Should Modes switch engines/models, or only text processing?** Default: text and post-process first; engine switching only among preloaded engines.

## Sources

- https://github.com/anomalyco/hex (shallow clone to /tmp/hexsrc, main branch). Files read: `README.md`, `Cargo.toml`, `docs/windows.md`, `docs/windows-signing.md`, `docs/linux.md`, `src/windows/context.rs`, `src/linux/context.rs`, `src/meeting/mcp.rs`, `src/updater/{macos,windows,linux}.rs`, `src/history.rs`, `src/personal_commands/script.rs`
- https://www.qt.io/licensing/open-source-lgpl-obligations
- https://github.com/KDAB/cxx-qt
- https://github.com/slint-ui/slint/blob/master/LICENSE.md
- Local: `git show origin/docs/competitor-hex:docs/ROADMAP-HEX-GAPS.md`, `/home/niteris/dev/talos-hq/HANDOFF-apple-notary.md`
- Not fetched: https://anoma.ly/hex/ (the repo was sufficient). Not read in full: COMPETITOR-HEX.md (only the roadmap derived from it), and the Azure walkthrough contents.

## Bottom line

talos-vox becomes a headless, cross-platform Rust daemon (`voxtype`, still the CLI/Waybar/compositor-keybind product on Linux) with a versioned local control protocol. Peers on that protocol:
- an MCP server (`voxtype mcp`),
- the existing TUI/OSD on Linux,
- a new pure-Rust egui + tray-icon desktop app (`voxtype-app`) that supervises the daemon on macOS and Windows.

All of it ships as notarized `.app`/DMG via Homebrew cask, Authenticode-signed per-user installers via WinGet, and the current Docker-built deb/rpm/AUR on Linux. Hex's Modes, history/paste-last and MCP are absorbed; QuickJS scripting and Annotate are skipped; the updater is deferred until signing exists. Qt is not adopted: cxx-qt still means QML, LGPL deploy and per-framework signing, and Hex itself proves a pure-Rust GPU UI is enough. Divergence stays mechanical by putting new work in new modules and features while upstreaming the general pieces. **First PR: `voxtype mcp` over stdio (rmcp, feature-gated) exposing `status` and `transcribe_file` through the existing control channel and `transcribe_file.rs`, with an integration test. No daemon changes.**
