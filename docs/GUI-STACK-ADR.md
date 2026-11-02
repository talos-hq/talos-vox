# ADR: Cross-platform GUI stack for talos-vox and talos-bar

Status: Proposed · Date: 2026-11-01 · Scope: analysis only, no repo changes

Measurement host: NixOS, Linux 6.18.48, Wayland (KDE Plasma, `WAYLAND_DISPLAY=wayland-0`), cargo through `soldr` (v0.9.50 shim). **No macOS or Windows numbers were measured. Every macOS or Windows figure below is marked `UNVERIFIED — needs a machine`.**

---

## Decision

**talos-vox uses Rust + egui for every app surface. That means `egui` 0.34 on `egui-wgpu`, with `tray-icon` and `tao`/`winit`, all on one event loop. The HUD is a separate surface: one thin native window shim per OS that draws with the same `egui-wgpu` renderer. talos-bar stays on Qt/QML for now, because it is a special case.**

- **Runner-up: Slint 1.x** (royalty-free licence, `.slint` DSL).
- **What flips the decision to Slint:** if the one-day egui text-input spike (S1 below) fails, switch talos-vox's *windows* (settings, history, model picker) to Slint. S1 fails if IME/CJK composition through fcitx5 or IBus on Wayland, or the macOS/Windows IME, does not commit correctly in an `egui::TextEdit`. It also fails if Orca, VoiceOver or Narrator cannot read and operate the settings form. The tray, popover and HUD stay on egui-wgpu either way.

## Why

1. **It is already the stack in the repo.** No other candidate starts from zero cost. talos-vox already depends on `egui 0.34`, `egui-wgpu 0.34`, `wgpu 29` (vulkan+gles), `tao 0.32` and `tray-icon 0.21.3` (`Cargo.toml:131-159`). `src/menubar.rs` is a 708-line tao + tray-icon tray, and `src/bin/voxtype_osd_native/` is an egui-wgpu layer-shell OSD. Choosing egui extends code the team already maintains. Choosing Qt, Slint, iced or Tauri adds a second UI stack before the first is removed.
2. **The team only writes Rust.** egui is plain Rust with no DSL, no QML, no JS and no Python. Immediate mode suits a small team: a settings window is a function, and there is no binding layer, codegen or property system to keep in sync. Slint (DSL) and Qt (QML + C++ bridge through cxx-qt) each add a second language. Tauri adds HTML/CSS/JS.
3. **One binary, nothing to bundle or sign beyond it.** The measured hello-window is 14.3 MB stripped. `ldd` shows only libc/libm/libgcc on Linux, because wgpu, winit and the fonts are statically linked and Vulkan/GL/Wayland are `dlopen`ed. On macOS this means one Mach-O to sign and notarize. The Qt alternative means signing every nested framework and plugin in the bundle (see Shipping). It also fits the existing Ubuntu-22.04 Docker release pipeline unchanged: it is just more Rust crates.
4. **The tray is the same API in every candidate, so it does not separate them.** On all three OSes the tray is `tray-icon`, whose docs say an event loop must run on the thread that owns it (the main thread on macOS). egui and winit own that loop, so the tray, popover window, settings window and HUD share a single loop. That is the "no second event-loop stack" requirement. Qt would replace tray-icon with `QSystemTrayIcon`, which is no better on Linux, where both end up on StatusNotifierItem/AppIndicator.
5. **The HUD needs a native window on every candidate, and egui-wgpu is the cheapest renderer to put in one.** No cross-platform toolkit sets layer-shell, `NSWindow.ignoresMouseEvents` + `sharingType = .none`, and `WS_EX_TRANSPARENT|WS_EX_NOACTIVATE` + `SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)` for you. You write a shim per OS whatever you pick. `egui-wgpu` renders into any `raw-window-handle` surface, and that is exactly how `voxtype_osd_native` already works on Wayland.
6. **The risk can be tested cheaply.** egui's known weak spots are IME, complex text and RTL, plus accessibility being thinner than Qt's. Both only matter for the boring windows, and the egui→Slint fallback swaps only those windows. The runner-up therefore costs one module, not a rewrite.
7. **It is maintained by more than one party.** egui is maintained by Emil Ernerfeldt and funded through rerun.io. wgpu is a gfx-rs/Mozilla/Firefox dependency, and winit is the community standard. Abandonment would need rerun.io and Firefox's WebGPU work to both stop.

## Scoring matrix

Markers: **M** = MEASURED here on Linux · **D** = DOC-QUOTED (see Sources) · **K** = known from the code read in this repo · **U** = UNVERIFIED, needs a machine or a spike.

| Requirement | egui/eframe | Qt 6 (cxx-qt / PySide6) | Slint 1.x | iced 0.14 | Tauri v2 | Xilem / gpui / Floem / Freya | Native per-OS shells | Split (HUD vs windows) |
|---|---|---|---|---|---|---|---|---|
| Linux build on NixOS + soldr | Builds, 1 crate set (**M**) | PySide6 build works only from the nixpkgs Qt; the PyPI wheel does not load (**K**, talos-bar.nix) | Builds (**M**) | Builds; the 0.14 `application()` API needed fn items instead of closures to type-check (**M**) | Not built; `webkit2gtk-4.1` 2.52.5 and `gtk+-3.0` 3.24.52 are present via pkg-config (**M**, deps only) | Not built (**U**) | n/a | = parts |
| Release size, Linux, stripped hello window | **14,250,936 B** (**M**) | talos-bar's Nix closure is **2.0 GiB** (**M**: `nix path-info -Sh`); a cxx-qt app would still ship QtCore/Gui/Qml/Quick, ~300 MB of qtbase + qtdeclarative store paths (**M**: 97 M + 190 M) | **20,374,536 B** (**M**) | **16,291,920 B** (**M**) | small binary plus system WebKitGTK (**U**) | **U** | GTK4 is a system lib (**U**) | ~egui |
| Cold start, Linux, 10 runs | mean **226 ms** (201–343) incl. wgpu adapter init (**M**) | not measured (**U**) | mean **58 ms** (54–68), default GL renderer (**M**) | mean **279 ms** (237–422) (**M**) | **U** | **U** | **U** | — |
| Runtime deps on Linux (`ldd`, beyond libc) | none; Vulkan/GL/Wayland are dlopened (**M**) | Qt libs, QML imports, `layer-shell-qt` plugin (**K**) | fontconfig, freetype, png, bz2, brotli, expat, zlib (**M**) | none (**M**) | WebKitGTK 4.1 + libsoup3 (**U**) | **U** | GTK (**U**) | — |
| macOS/Windows size & start | **U** | **U** | **U** | **U** | **U** | **U** | **U** | **U** |
| Tray icon | `tray-icon`: SNI/AppIndicator on Linux (GTK3 + libappindicator by default, or the `ksni` backend), NSStatusItem, Shell_NotifyIcon (**D**) | QSystemTrayIcon (SNI on Linux) (**U** cross-OS) | none built in; use `tray-icon` (**U**) | none built in; use `tray-icon` (**U**) | built in, wraps the same `tray-icon` (**D** via tray-icon repo) | none (**U**) | native (**U**) | egui side |
| Popover anchored to the tray | tray-icon gives the click rect; you place a borderless window there. Works on mac/Win (**U**). On Wayland a client cannot learn the icon's position (protocol fact) | same limitation; QML Popup in its own window (**U**) | same (**U**) | same (**U**) | same, via a webview window (**U**) | **U** | NSPopover on mac is the only real anchored popover; Win11 flyout via XAML (**U**) | egui |
| Single event loop for tray + windows | yes: tray-icon needs a loop on its thread, which winit provides (**D**); talos-vox's menubar already does this with tao (**K**) | yes, Qt loop (**K**) | yes, winit backend (**U**) | yes (**U**) | yes (**U**) | **U** | per OS | yes |
| GPU backend | wgpu: Metal / DX12 / Vulkan / GLES; this repo builds wgpu with `vulkan`,`gles` only (**K**) | RHI: Metal / D3D11 / Vulkan / GL (**U**) | GL by default, Skia optional (**U**); this build linked freetype/fontconfig (**M**) | wgpu (**U**), tiny-skia fallback (**U**) | the webview's compositor (**U**) | wgpu/vello, gpui own Metal/Blade (**U**) | native | — |
| No-GPU fallback (VM/RDP) | wgpu GLES → llvmpipe; on Windows, the WARP adapter (**U**) | software Quick backend (**U**) | software renderer feature (**U**) | tiny-skia (**U**) | webview's own (**U**) | **U** | GDI etc. | — |
| HUD: click-through, transparent, topmost, no focus steal, hidden from capture | needs a per-OS shim. On Linux, layer-shell is done today in `voxtype_osd_native` (**K**); mac/Win **U** | layer-shell-qt on Linux (**K**, talos-bar); mac/Win shim (**U**) | needs a shim (**U**) | needs a shim (**U**) | transparent webview windows are feasible; click-through and capture exclusion still need native calls (**U**) | **U** | native (**U**) | shim per OS |
| IME / CJK / dead keys / RTL / emoji | IME through winit; RTL/bidi is weak; emoji font included (**D**: README lists "emoji supports", IME credit) — **S1 spike** | best of the field (**U**) | own text stack (**U**) | IME added recently (**U**) | browser-grade (**U**) | **U** | native-grade (**U**) | — |
| Accessibility | AccessKit, enabled by default in eframe. The README says it "implements the native accessibility APIs on Windows and macOS" (**D**). AccessKit now also has an AT-SPI adapter (**D**) | Qt a11y on all three OSes (**U**) | AccessKit (**U**) | AccessKit in progress (**U**) | ARIA through the webview (**U**) | **U** | native | — |
| Signing / notarization | one binary (**U** mac) | every nested Qt framework and plugin `.dylib` in the bundle (**U**) | one binary (**U**) | one binary (**U**) | one binary; WebView2 bootstrapper on Windows (**U**) | **U** | Swift/C# targets per OS (**U**) | one binary |
| Licence | MIT/Apache | LGPLv3: dynamic linking plus relinkability obligations; PySide6 is LGPL (**U**) | "Royalty-Free: Build proprietary applications excluding Embedded Systems"; GPLv3; commercial (**D**) | MIT | MIT/Apache | MIT/Apache (gpui Apache) | — | — |
| Team fit (Rust-only) | Rust only | QML + C++ (cxx-qt) or Python | Rust + `.slint` DSL | Rust (Elm architecture, heavier generics, see the iced build errors above, **M**) | Rust + web front end | Rust, but pre-1.0 | Swift + C# + Rust | Rust |
| Longevity | rerun.io-backed, active | Qt Company | SixtyFPS GmbH: one company with commercial licences | one lead maintainer (hecrj); System76's COSMIC uses a fork | Tauri programme (CrabNebula) | Xilem is research (Linebender), gpui is tied to Zed, Floem to Lapce, Freya is solo | OS vendors | — |

Observations that should drive the decision:

- **Size is a wash.** egui (14.3 MB), iced (16.3 MB) and Slint (20.4 MB) are within 6 MB of each other. Qt is two orders of magnitude bigger on NixOS.
- **Slint starts fastest: 58 ms against egui's 226 ms (M).** Most of egui's time is wgpu adapter and device creation. A tray app pays that **once at login**, not each time the popover opens, because the window is pre-created and hidden. Popover latency is the number that matters, and it is a show/hide plus one frame (S3 spike).

## talos-bar verdict

**Keep Qt for now. Do not migrate. Revisit after talos-vox has a working Rust popover on egui.**

What I found (files read: `pyproject.toml`, `src/talos_bar/**`, `flake.nix`, `~/dev/nixos/system/talos-bar.nix`):

- **Size of the code:** 1,917 lines of Python (`app.py` 384, `search.py` 145, `platform/__init__.py` 152, the glue modules, and three carried-over scripts) and 1,290 lines of QML across 17 files. The biggest file is `Agenda.qml` at 345 lines; `Bar.qml` is 178 and `SearchPopup.qml` 137.
- **Payload:** the installed derivation's closure is **2.0 GiB (M)**. The heaviest paths are qttools, qt3d, qtquick3d, qtgraphs and gst-plugins-bad, at 720–810 MiB closures each, and 32 qt/pyside/python store paths. This is because nixpkgs `pyside6` pulls in all of Qt. The flake works around it by stripping `pyside6-essentials` (`pythonRemoveDeps`) and re-adding nixpkgs `pyside6`.
- **The NixOS wheel problem:** `talos-bar.nix` says "the PyPI PySide6 wheel does not load on NixOS". `platform/__init__.py` probes for `liblayer-shell.so` because "PySide6 from PyPI does not ship it". The flake has to clear and rebuild `QT_PLUGIN_PATH`, `QML2_IMPORT_PATH` and the other import paths (`flake.nix:10,30,63-65`). That is the kind of environment-variable plumbing a single static binary does not need.
- **The restart hack:** the unit uses `Restart=always` because the bar exited 0 after 15 s. The probable cause is Qt quitting when its layer-shell surface went away. The code already sets `setQuitOnLastWindowClosed(False)` (`app.py:360`), so the root cause is still open. That is a Qt and layer-shell-qt lifecycle bug, not a Python one.
- **Per-platform shims:** `platform/__init__.py` covers only Linux layer-shell versus a "frameless window at the top centre". Its docstring says "X11 struts, the Windows AppBar and the macOS NSPanel land per OS as described in docs/DESIGN.md", meaning they are **not written yet**. `glue/__init__.py:200` and `search.py:24` contain the only `win32`/`darwin` branches.

**Why the bar is a special case:** it is a bar, not an app. It is a docked strip with exclusive-zone reservation plus drop-down cards. On Linux the hard part is layer-shell, and `layer-shell-qt` does that well today, including the input-region masking in `WaylandPopup.qml`. A tray-first dictation app has a different shape.

**Cost of migrating it to egui:**
- Re-express the 1,290 lines of QML: bar, tabs, the agenda list with markdown rendering, search popup, drop animation and theme.
- Port the ~650 lines of Python glue (`app.py` model/placement, agenda, clock, tasks, search), keeping the three scripts as subprocesses.
- Write a layer-shell host for egui with exclusive zone, input rect and multiple surfaces. talos-vox's `voxtype_osd_native` is a starting point, but it renders one overlay, not a bar with child popups.
- Estimate: 2–3 engineer-weeks. Expected gain: the closure drops from 2.0 GiB to ~15 MB, the env-var plumbing goes away, and the restart hack probably does too. **U** until the S5 spike runs.

**Hybrid now:** have talos-bar adopt only the shared *theme tokens* (a TOML file both read). Migrate it after S5 passes.

## talos-vox verdict

**Shape:**

```
crates/
  talos-ui/            # shared (see next section): theme, widgets, popover chrome
  voxtype-app/         # bin `voxtype-app`: tray + popover + settings/history/models windows + HUD
    src/main.rs        # one winit EventLoop; owns TrayIcon; all windows are egui viewports
    src/tray.rs        # moves out of src/menubar.rs (tao → winit, keeps tray-icon)
    src/popover.rs     # borderless, pre-created, hidden window; shown at tray rect
    src/windows/{settings,history,models}.rs
    src/hud/mod.rs     # trait HudSurface { show(state), hide(), place(monitor) }
    src/platform/linux_layer_shell.rs   # from src/bin/voxtype_osd_native (wlr-layer-shell)
    src/platform/linux_x11.rs           # override-redirect + XShape empty input region
    src/platform/macos.rs               # NSPanel: nonactivating, ignoresMouseEvents, sharingType=.none, level=.statusBar
    src/platform/windows.rs             # WS_EX_LAYERED|TRANSPARENT|NOACTIVATE|TOPMOST|TOOLWINDOW + WDA_EXCLUDEFROMCAPTURE
```

- **Daemon boundary:** the daemon stays a headless process. `voxtype-app` is a client over the existing control channel and state file (`voxtype status --follow` semantics), so a GUI crash never loses a recording.
- **Tray popover:** an `egui` viewport built with `with_decorations(false)`, `with_window_level(AlwaysOnTop)` and `skip_taskbar`. It is created hidden at start-up and positioned from `TrayIconEvent::Click { rect, .. }`. It closes on `WindowEvent::Focused(false)` and on Esc, and egui's focus traversal gives keyboard navigation.
  - **macOS:** anchoring works (**U**).
  - **Windows:** anchoring works, and the window must also be placed above the taskbar edge (**U**).
  - **Linux:** honestly, this does **not** work on Wayland. A client cannot learn the tray icon's position. Fallbacks:
    - On Wayland, the HUD-class layer-shell surface anchored top-right/bottom-right next to the panel.
    - On X11, the SNI-reported geometry when it exists, otherwise the cursor position.
    - On GNOME without AppIndicator, there is no tray at all. A global shortcut opens the popover centred.
- **Replacing the three OSDs:**
  - `voxtype_osd_native` (egui+wgpu on layer-shell) **becomes** `platform/linux_layer_shell.rs`.
  - `voxtype_osd_gtk4` and the Quickshell QML plugin are **deprecated** for one minor release, then removed. Keep the Quickshell plugin for Quickshell users only if the issue tracker shows real users; it consumes the same status feed, so it costs nothing to leave.
  - **Layer-shell stays required on Linux.** It is the only way a Wayland compositor gives you an overlay that is topmost, never focused and click-through (empty input region). GNOME lacks it, so on GNOME the HUD falls back to no HUD, with the tray icon state plus an optional notification.
- **macOS today:** `src/menubar.rs` (tao + tray-icon) is folded into `voxtype-app`, so macOS gets the same windows as Linux and Windows.
- **What stays:** the ratatui TUI (`voxtype configure`) remains the headless and SSH path. The CLI remains the primary interface, as CLAUDE.md requires.

## The shared piece

**Yes, but keep it small:** one `talos-ui` crate with

1. **Theme tokens.** Colours, radii, spacing and the font stack, loaded from a TOML file that Python (talos-bar today) can also read.
2. **`Popover` chrome.** Frame, shadow, focus-loss dismissal, Esc handling and "place at rect, clamp to the monitor work area".
3. **`HudSurface` trait plus the four platform shims.** This is the most expensive part to write and the most reusable. talos-bar's Meta+Space search popup is a HUD-class surface too.
4. **About five settings widgets:** toggle row, choice row, path picker, hotkey capture and a section header.

**Do not share** app state, tray menus or window management policy. **It pays for itself** at the point talos-bar ports its search popup, its first Rust surface. Until then it is a crate with one consumer and should live inside talos-vox at `crates/talos-ui`.

## Migration and spikes

There is one day per spike. Each spike runs in `/tmp` or a branch prototype, never in the release build.

| # | Risk | Spike | Pass | Fail → action |
|---|---|---|---|---|
| S1 | egui text input and accessibility for settings | egui form with a TextEdit; test fcitx5-mozc and IBus on KDE Wayland, macOS Japanese IME, Windows MS-IME; also Orca, VoiceOver and Narrator reading and toggling 5 controls | commits CJK correctly, dead keys work, paste works, all 5 controls are read and operable on all 3 OSes | switch the windows (not the tray, popover or HUD) to Slint |
| S2 | macOS HUD | NSPanel through `objc2` around a wgpu surface | click-through, no focus steal from TextEdit.app, not visible in a QuickTime screen recording | fall back to a plain egui window with `ignoresMouseEvents` and accept capture visibility |
| S3 | popover latency | pre-created hidden viewport, open from a tray click, timestamped | first frame visible < 100 ms on Linux X11/Wayland, mac and Win | keep the window mapped off-screen instead of hidden |
| S4 | no-GPU machines | run the app under a Windows RDP session, a macOS VM, and Linux with `WGPU_BACKEND=gl` + llvmpipe | renders at all and uses < 5 % CPU idle | build with `egui_glow` as a fallback renderer |
| S5 | Rust bar viability (talos-bar) | egui on layer-shell with exclusion zone + an input-region popup, left running for 8 h | no exit, no restart needed, correct reserved space | talos-bar stays on Qt permanently; fix the exit bug inside Qt instead |
| S6 | Windows HUD and capture exclusion | layered window + `WDA_EXCLUDEFROMCAPTURE` + wgpu DX12 | hidden in a Teams/OBS capture, never takes focus | use DirectComposition, or accept that the HUD is visible in captures |

**First PR:** `crates/voxtype-app` skeleton. It moves `src/menubar.rs` from tao to the winit loop with tray-icon, adds an empty pre-created popover that toggles on tray click, and gates on `soldr cargo clippy -- -D warnings` plus building in `Dockerfile.build`. No HUD and no settings yet.

**Order after that:**
1. S1 + S3
2. Settings window
3. Move the Linux HUD to the `HudSurface` trait (S2 and S6 in parallel)
4. History and model windows
5. Deprecate the GTK4 OSD
6. S5 decides talos-bar

## Risks

| Risk | Probability | Blast radius | Cheapest falsifier |
|---|---|---|---|
| egui IME/RTL is not good enough | medium | settings window only | S1 |
| Wayland gives no anchored popover | certain | Linux tray UX | already accepted; the fallback is specified above |
| tray-icon on Linux pulls in GTK3 + libappindicator | certain on the default backend (**D**) | Linux deps and packaging | use the `ksni` backend, which the docs say needs no such libraries (**D**) |
| wgpu start-up cost (226 ms measured) and driver bugs on old Intel | medium | first open after login, VMs | S3/S4; egui_glow fallback |
| macOS notarization of the wgpu/objc2 binary | low | mac release | sign + `notarytool` one hello-world build (**U**) |
| egui breaking API each minor release | high | churn of hours per bump | pin the version, upgrade once per milestone |
| SixtyFPS licence change, if we fall back to Slint | low | windows only | royalty-free terms as quoted; keep the GPL option in mind |
| The talos-bar Qt exit bug recurs | already happening | top edge empty | `Restart=always` stays; add `QT_LOGGING_RULES=qt.qpa.wayland*=true` logging to capture it |

## Sources

Local files read:
- `~/dev/talos-hq/hermes-slim-extern/talos-bar/pyproject.toml`, `flake.nix`, `src/talos_bar/platform/__init__.py`, `src/talos_bar/qml/WaylandPopup.qml`; line counts and grep across all `src/talos_bar/**/*.py|*.qml`
- `~/dev/nixos/system/talos-bar.nix`
- `talos-vox/Cargo.toml` (UI deps), `src/menubar.rs` (line count), `src/bin/` listing
- Nix closure: `nix path-info -rSh /nix/store/qk0ibicljhva2mchiqfgc09bh6nkswsh-talos-bar-0.0.1`

Prototypes: `/tmp/xgui/{egui,iced,slint}`, built with `soldr cargo build --release`, `strip=true`, timed with a 10-run `date +%s%N` loop that exits after the first event-loop turn.

URLs fetched:
- https://github.com/tauri-apps/tray-icon
- https://slint.dev/pricing
- https://github.com/emilk/egui
- https://github.com/AccessKit/accesskit

Not fetched (claims marked **U**): Qt licensing, Tauri, iced, Xilem/gpui/Floem/Freya docs, Apple and Microsoft API docs.
