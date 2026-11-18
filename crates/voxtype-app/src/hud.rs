//! HUD: a small always-on-top, click-through overlay with the live
//! waveform and level meter while the daemon records or transcribes.
//!
//! It reads the same feed as Linux's `voxtype-osd-*` frontends: 16-byte
//! audio frames at 100 Hz from the daemon's level socket, through
//! `voxtype::osd::ipc` (Unix socket on macOS, kernal-api named pipe on
//! Windows), and the daemon state from the control protocol. Drawing reuses
//! `voxtype::osd::visual` (envelope projection, meter zones, palette).
//!
//! It runs as its own process (`voxtype-app --hud`) so it gets a root
//! eframe window, whose native handle is reachable through
//! `raw-window-handle`. eframe/winit cover decorations, always-on-top and
//! click-through; the rest is set on the native window:
//! - macOS (objc2-app-kit): status-bar window level, joins every Space and
//!   full-screen apps, no shadow, ignores mouse events. The window is
//!   transparent; when idle nothing is drawn. The process runs with the
//!   Accessory activation policy, so it adds no Dock icon.
//! - Windows (windows-sys): tool window (no taskbar or Alt-Tab entry),
//!   no-activate, topmost, click-through. wgpu swapchains have no reliable
//!   alpha on Windows, so the window is opaque, shaped to a rounded pill
//!   with a window region, and hidden/shown with `ShowWindow` from the
//!   status thread (a hidden winit window gets no frames to do it from).
//!
//! The app spawns the HUD on macOS and Windows only. Linux keeps
//! `voxtype-osd-*`; `--hud` still runs there, for testing the drawing.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui;
use voxtype::audio::levels::{AudioFrame, FRAME_HZ};
use voxtype::osd::ipc::FrameRing;
use voxtype::osd::visual::{
    peak_meter_fraction, project_envelope, Color, EnvelopeColumn, MeterZone, Palette, PeakHold,
};

/// Logical size of the HUD.
pub const SIZE: [f32; 2] = [320.0, 64.0];
/// Gap between the HUD and the bottom of the screen.
const BOTTOM_MARGIN: f32 = 96.0;
/// Seconds of audio across the waveform.
const WINDOW_SECS: f32 = 3.0;
const STATUS_INTERVAL: Duration = Duration::from_millis(150);

/// Whether the HUD shows for a daemon state word.
pub fn shown_for(state: &str) -> bool {
    matches!(state, "recording" | "transcribing")
}

/// What the threads feed the window.
pub struct Shared {
    pub ring: Mutex<FrameRing>,
    pub peak: Mutex<PeakHold>,
    /// The daemon's state word, empty when unreachable.
    pub state: Mutex<String>,
}

impl Shared {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            ring: Mutex::new(FrameRing::for_window(WINDOW_SECS)),
            peak: Mutex::new(PeakHold::new(20.0)),
            state: Mutex::new(String::new()),
        })
    }

    fn state(&self) -> String {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn push(&self, frame: AudioFrame) {
        self.ring
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(frame);
    }
}

/// `voxtype-app --hud`.
pub fn run(exit_with_parent: bool) -> eframe::Result {
    if exit_with_parent {
        // The app holds our stdin; EOF means it is gone (crashed or quit
        // without stopping us), so go too.
        std::thread::spawn(|| {
            use std::io::Read;
            let mut buf = [0u8; 64];
            while matches!(std::io::stdin().read(&mut buf), Ok(n) if n > 0) {}
            std::process::exit(0);
        });
    }
    let viewport = egui::ViewportBuilder::default()
        .with_title("Voxtype HUD")
        .with_app_id("voxtype-hud")
        .with_inner_size(SIZE)
        .with_resizable(false)
        .with_decorations(false)
        .with_transparent(platform::TRANSPARENT)
        .with_always_on_top()
        .with_mouse_passthrough(true)
        .with_taskbar(false)
        .with_active(false)
        // Off screen until the first frame has placed it and set the
        // native flags, so a not-yet-click-through window never flashes.
        .with_position([-10_000.0, -10_000.0]);
    #[allow(unused_mut)]
    let mut options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    // An accessory process: no Dock icon or menu bar of its own next to
    // the app's.
    #[cfg(target_os = "macos")]
    {
        use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};
        options.event_loop_builder = Some(Box::new(|builder| {
            builder.with_activation_policy(ActivationPolicy::Accessory);
        }));
    }
    eframe::run_native(
        "Voxtype HUD",
        options,
        Box::new(|cc| {
            let shared = Shared::new();
            let native = Arc::new(platform::Native::default());
            spawn_levels(
                shared.clone(),
                voxtype::osd::ipc::resolve_socket_path(None),
                cc.egui_ctx.clone(),
            );
            spawn_status(
                shared.clone(),
                voxtype::control::socket_path(),
                native.clone(),
                cc.egui_ctx.clone(),
            );
            Ok(Box::new(Hud::new(shared, native)))
        }),
    )
}

/// Audio frames from the level socket, reconnecting as the Linux OSD does.
fn spawn_levels(shared: Arc<Shared>, socket: PathBuf, ctx: egui::Context) {
    std::thread::Builder::new()
        .name("voxtype-hud-levels".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("tokio runtime for the level socket");
            rt.block_on(voxtype::osd::ipc::run_ipc_loop(socket, 0.5, move |frame| {
                shared.push(frame);
                ctx.request_repaint();
            }));
        })
        .expect("spawn the level thread");
}

/// The daemon state, polled; shows and hides the native window where
/// that has to happen outside a frame.
fn spawn_status(
    shared: Arc<Shared>,
    socket: PathBuf,
    native: Arc<platform::Native>,
    ctx: egui::Context,
) {
    std::thread::Builder::new()
        .name("voxtype-hud-status".into())
        .spawn(move || loop {
            let state = voxtype::control::request_v1(&socket, "status")
                .ok()
                .and_then(|r| crate::client::parse_reply(&r).ok())
                .unwrap_or_default();
            let changed = {
                let mut cur = shared.state.lock().unwrap_or_else(|e| e.into_inner());
                let changed = *cur != state;
                *cur = state.clone();
                changed
            };
            if changed {
                native.show(shown_for(&state));
                ctx.request_repaint();
            }
            std::thread::park_timeout(STATUS_INTERVAL);
        })
        .expect("spawn the status thread");
}

pub struct Hud {
    shared: Arc<Shared>,
    native: Arc<platform::Native>,
    palette: Palette,
    placed: bool,
    last_tick: Instant,
}

impl Hud {
    pub fn new(shared: Arc<Shared>, native: Arc<platform::Native>) -> Self {
        Self {
            shared,
            native,
            palette: Palette::default(),
            placed: false,
            last_tick: Instant::now(),
        }
    }

    /// Bottom centre of the monitor, once its size is known.
    fn place(&mut self, ctx: &egui::Context) {
        if self.placed {
            return;
        }
        let Some(monitor) = ctx.input(|i| i.viewport().monitor_size) else {
            return;
        };
        let pos = egui::pos2(
            ((monitor.x - SIZE[0]) / 2.0).max(0.0),
            (monitor.y - SIZE[1] - BOTTOM_MARGIN).max(0.0),
        );
        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(pos));
        self.placed = true;
    }

    /// Draw one frame. Separate from `eframe::App` so tests can run it on a
    /// bare `egui::Context`.
    pub fn show(&mut self, ui: &mut egui::Ui) {
        let state = self.shared.state();
        if !shown_for(&state) {
            return;
        }
        let now = Instant::now();
        let dt = now.duration_since(self.last_tick).as_secs_f32();
        self.last_tick = now;

        let rect = ui.max_rect();
        let painter = ui.painter().clone();
        let bg = color(self.palette.background.with_alpha(0.88));
        painter.rect_filled(rect, rect.height() / 2.0, bg);

        let inner = rect.shrink2(egui::vec2(rect.height() / 2.0, 10.0));
        let label_w = 92.0;
        let meter_w = 10.0;
        let label_rect = egui::Rect::from_min_size(inner.min, egui::vec2(label_w, inner.height()));
        let meter_rect =
            egui::Rect::from_min_max(egui::pos2(inner.right() - meter_w, inner.top()), inner.max);
        let wave_rect = egui::Rect::from_min_max(
            egui::pos2(label_rect.right() + 8.0, inner.top()),
            egui::pos2(meter_rect.left() - 8.0, inner.bottom()),
        );

        let (dot, text) = if state == "recording" {
            (egui::Color32::from_rgb(0xd9, 0x3f, 0x3f), "Recording")
        } else {
            (egui::Color32::from_rgb(0xe0, 0xa0, 0x30), "Transcribing")
        };
        painter.circle_filled(
            egui::pos2(label_rect.left() + 5.0, label_rect.center().y),
            5.0,
            dot,
        );
        painter.text(
            egui::pos2(label_rect.left() + 16.0, label_rect.center().y),
            egui::Align2::LEFT_CENTER,
            text,
            egui::FontId::proportional(14.0),
            color(self.palette.foreground),
        );

        let (envelope, peak) = {
            let ring = self.shared.ring.lock().unwrap_or_else(|e| e.into_inner());
            let frames: Vec<AudioFrame> = ring.iter().collect();
            let cols = wave_rect.width().max(16.0) as usize;
            (
                project_envelope(
                    &frames,
                    cols,
                    WINDOW_SECS as f64 * FRAME_HZ as f64,
                    ring.scroll_offset(now),
                ),
                ring.latest().map(|f| f.peak_dbfs).unwrap_or(-120.0),
            )
        };
        let held = {
            let mut hold = self.shared.peak.lock().unwrap_or_else(|e| e.into_inner());
            hold.update(peak, dt);
            hold.held_dbfs
        };
        draw_waveform(&painter, wave_rect, &self.palette, &envelope);
        draw_meter(&painter, meter_rect, &self.palette, peak, held);
        // Keep scrolling between audio deliveries.
        ui.ctx().request_repaint_after(Duration::from_millis(16));
    }
}

impl eframe::App for Hud {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        platform::CLEAR
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.native.attach(frame, ui.ctx());
        self.place(ui.ctx());
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show_inside(ui, |ui| self.show(ui));
    }
}

fn color(c: Color) -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(
        (c.r.clamp(0.0, 1.0) * 255.0) as u8,
        (c.g.clamp(0.0, 1.0) * 255.0) as u8,
        (c.b.clamp(0.0, 1.0) * 255.0) as u8,
        (c.a.clamp(0.0, 1.0) * 255.0) as u8,
    )
}

fn draw_waveform(
    painter: &egui::Painter,
    rect: egui::Rect,
    palette: &Palette,
    envelope: &[EnvelopeColumn],
) {
    let mid = rect.center().y;
    let half = rect.height() * 0.48;
    painter.line_segment(
        [egui::pos2(rect.left(), mid), egui::pos2(rect.right(), mid)],
        egui::Stroke::new(1.0_f32, color(palette.foreground.with_alpha(0.25))),
    );
    if envelope.is_empty() {
        return;
    }
    let step = rect.width() / envelope.len() as f32;
    let fill = color(palette.accent);
    // One thin bar per column: concave shapes need no tessellation.
    for (i, col) in envelope.iter().enumerate() {
        let x = rect.left() + (i as f32 + 0.5) * step;
        let top = mid - col.max.clamp(-1.0, 1.0) * half;
        let bot = mid - col.min.clamp(-1.0, 1.0) * half;
        if (bot - top).abs() >= 0.5 {
            painter.line_segment(
                [egui::pos2(x, top), egui::pos2(x, bot)],
                egui::Stroke::new(step.max(1.0), fill),
            );
        }
    }
}

fn draw_meter(
    painter: &egui::Painter,
    rect: egui::Rect,
    palette: &Palette,
    peak_dbfs: f32,
    held_dbfs: f32,
) {
    const SEGMENTS: usize = 8;
    const FLOOR: f32 = -60.0;
    let seg_h = rect.height() / SEGMENTS as f32;
    let lit = (peak_meter_fraction(peak_dbfs, FLOOR) * SEGMENTS as f32).round() as usize;
    for i in 0..SEGMENTS {
        let r = egui::Rect::from_min_max(
            egui::pos2(rect.left(), rect.bottom() - (i as f32 + 1.0) * seg_h + 1.0),
            egui::pos2(rect.right(), rect.bottom() - i as f32 * seg_h - 1.0),
        );
        let zone = MeterZone::from_dbfs(FLOOR * (1.0 - i as f32 / SEGMENTS as f32)).color(palette);
        let c = if i < lit { zone } else { zone.with_alpha(0.18) };
        painter.rect_filled(r, 1.0, color(c));
    }
    let held = peak_meter_fraction(held_dbfs, FLOOR);
    if held > 0.0 {
        let y = rect.bottom() - held * rect.height();
        painter.rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(rect.left(), y - 1.0),
                egui::pos2(rect.right(), y + 1.0),
            ),
            0.0,
            color(palette.foreground),
        );
    }
}

/// Native window flags beyond what eframe exposes.
#[cfg(target_os = "macos")]
mod platform {
    use std::sync::Mutex;

    use eframe::egui;
    use objc2::rc::Retained;
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSView, NSWindow, NSWindowCollectionBehavior};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    pub const TRANSPARENT: bool = true;
    pub const CLEAR: [f32; 4] = [0.0; 4];
    /// kCGStatusWindowLevel: above normal and floating windows, like a
    /// menu-bar extra's popover.
    const STATUS_WINDOW_LEVEL: isize = 25;

    #[derive(Default)]
    pub struct Native {
        done: Mutex<bool>,
    }

    impl Native {
        /// Once, on the main thread: set the window's level and behaviour.
        pub fn attach(&self, frame: &eframe::Frame, _ctx: &egui::Context) {
            let mut done = self.done.lock().unwrap_or_else(|e| e.into_inner());
            if *done {
                return;
            }
            *done = true;
            let Some(_mtm) = MainThreadMarker::new() else {
                return;
            };
            let Ok(handle) = frame.window_handle() else {
                return;
            };
            let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
                return;
            };
            // SAFETY: winit hands out a live NSView for its window, and this
            // runs on the main thread (checked above).
            let view: Retained<NSView> =
                unsafe { Retained::retain(appkit.ns_view.as_ptr().cast()) }
                    .expect("winit's NSView");
            let Some(window): Option<Retained<NSWindow>> = view.window() else {
                return;
            };
            window.setLevel(STATUS_WINDOW_LEVEL);
            window.setCollectionBehavior(
                NSWindowCollectionBehavior::CanJoinAllSpaces
                    | NSWindowCollectionBehavior::Stationary
                    | NSWindowCollectionBehavior::IgnoresCycle
                    | NSWindowCollectionBehavior::FullScreenAuxiliary,
            );
            window.setIgnoresMouseEvents(true);
            window.setHasShadow(false);
        }

        /// Transparent when idle; nothing to do.
        pub fn show(&self, _visible: bool) {}
    }
}

#[cfg(windows)]
mod platform {
    use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

    use eframe::egui;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::Graphics::Gdi::{CreateRoundRectRgn, SetWindowRgn};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetLayeredWindowAttributes, SetWindowLongPtrW, SetWindowPos, ShowWindow,
        GWL_EXSTYLE, HWND_TOPMOST, LWA_ALPHA, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE,
        SWP_NOSIZE, SW_HIDE, SW_SHOWNOACTIVATE, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
        WS_EX_TOPMOST, WS_EX_TRANSPARENT,
    };

    pub const TRANSPARENT: bool = false;
    /// The pill's colour outside the drawn shape, cut away by the region.
    pub const CLEAR: [f32; 4] = [0.07, 0.07, 0.09, 1.0];

    #[derive(Default)]
    pub struct Native {
        hwnd: AtomicIsize,
        /// The last show/hide asked for before the window existed.
        want_visible: AtomicBool,
    }

    impl Native {
        /// Once: extended styles, a rounded region, then hide if idle.
        pub fn attach(&self, frame: &eframe::Frame, ctx: &egui::Context) {
            if self.hwnd.load(Ordering::Acquire) != 0 {
                return;
            }
            let Ok(handle) = frame.window_handle() else {
                return;
            };
            let RawWindowHandle::Win32(win32) = handle.as_raw() else {
                return;
            };
            let hwnd = win32.hwnd.get() as HWND;
            let px = |v: f32| (v * ctx.pixels_per_point()).round() as i32;
            let (w, h) = (px(super::SIZE[0]), px(super::SIZE[1]));
            // SAFETY: a live HWND owned by this process's UI thread.
            unsafe {
                let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
                let ex = ex
                    | WS_EX_TOOLWINDOW
                    | WS_EX_NOACTIVATE
                    | WS_EX_TOPMOST
                    | WS_EX_LAYERED
                    | WS_EX_TRANSPARENT;
                SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex as isize);
                // A layered window shows nothing until it has attributes.
                SetLayeredWindowAttributes(hwnd, 0, 255, LWA_ALPHA);
                SetWindowRgn(hwnd, CreateRoundRectRgn(0, 0, w + 1, h + 1, h, h), 1);
                SetWindowPos(
                    hwnd,
                    HWND_TOPMOST,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_FRAMECHANGED,
                );
            }
            self.hwnd.store(hwnd as isize, Ordering::Release);
            self.show(self.want_visible.load(Ordering::Acquire));
        }

        /// Called from the status thread; ShowWindow posts to the window's
        /// thread, so this is safe off it.
        pub fn show(&self, visible: bool) {
            self.want_visible.store(visible, Ordering::Release);
            let hwnd = self.hwnd.load(Ordering::Acquire);
            if hwnd == 0 {
                return;
            }
            // SAFETY: the HWND stays valid for the process's lifetime.
            unsafe {
                ShowWindow(
                    hwnd as HWND,
                    if visible { SW_SHOWNOACTIVATE } else { SW_HIDE },
                );
            }
        }
    }
}

/// Elsewhere (Linux, for testing the drawing): eframe's flags only.
#[cfg(not(any(target_os = "macos", windows)))]
mod platform {
    use eframe::egui;

    pub const TRANSPARENT: bool = true;
    pub const CLEAR: [f32; 4] = [0.0; 4];

    #[derive(Default)]
    pub struct Native {
        _none: (),
    }

    impl Native {
        pub fn attach(&self, _frame: &eframe::Frame, _ctx: &egui::Context) {}
        pub fn show(&self, _visible: bool) {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shows_only_while_busy() {
        assert!(shown_for("recording"));
        assert!(shown_for("transcribing"));
        assert!(!shown_for("idle"));
        assert!(!shown_for(""));
    }

    #[test]
    fn renders_headless_in_every_state() {
        let shared = Shared::new();
        for seq in 0..200u32 {
            let level = (seq as f32 / 10.0).sin() * 0.6;
            shared.push(AudioFrame {
                seq,
                min: -level.abs(),
                max: level.abs(),
                peak_dbfs: -12.0,
            });
        }
        let mut hud = Hud::new(shared.clone(), Arc::new(platform::Native::default()));
        let ctx = egui::Context::default();
        for state in ["", "idle", "recording", "transcribing"] {
            *shared.state.lock().unwrap() = state.into();
            let out = ctx.run_ui(egui::RawInput::default(), |ui| hud.show(ui));
            let drew = !out.shapes.is_empty();
            assert_eq!(drew, shown_for(state), "state {state:?}");
        }
    }
}
