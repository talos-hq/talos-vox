//! The main window: a Home tab (daemon state, start/stop, history), and
//! the Settings and Modes editors. Also drives the tray and owns the
//! daemon supervisor.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};

use eframe::egui;

use crate::client::{DaemonState, History, Request, Snapshot};
use crate::models::Models;
use crate::modes::Modes;
use crate::settings::Settings;
use crate::supervisor::{Status, Supervisor};
#[cfg(any(
    target_os = "macos",
    windows,
    all(target_os = "linux", feature = "linux-tray")
))]
use crate::tray;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Home,
    Settings,
    Modes,
    Models,
}

impl Tab {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "home" => Some(Tab::Home),
            "settings" => Some(Tab::Settings),
            "modes" => Some(Tab::Modes),
            "models" => Some(Tab::Models),
            _ => None,
        }
    }
}

pub struct App {
    requests: Sender<Request>,
    snapshots: Receiver<Snapshot>,
    snap: Snapshot,
    tab: Tab,
    /// The config file the editors write; `None` when no config dir exists.
    config_path: Option<PathBuf>,
    // Loaded when their tab is first opened.
    settings: Option<Settings>,
    modes: Option<Modes>,
    models: Option<Models>,
    /// The voxtype binary model downloads run.
    voxtype: PathBuf,
    supervisor: Supervisor,
    /// The HUD process, stopped on exit.
    hud: Option<std::process::Child>,
    #[cfg(any(
        target_os = "macos",
        windows,
        all(target_os = "linux", feature = "linux-tray")
    ))]
    tray: Option<(tray::Tray, Receiver<tray::Action>)>,
    /// Closing the window hides it to the tray instead of quitting. Set by
    /// `with_tray`, unless the window system can't hide a window (Wayland).
    hide_on_close: bool,
    /// The tray's Quit was clicked: let the next close through.
    quitting: bool,
}

impl App {
    pub fn new(
        requests: Sender<Request>,
        snapshots: Receiver<Snapshot>,
        config_path: Option<PathBuf>,
    ) -> Self {
        Self {
            requests,
            snapshots,
            snap: Snapshot::default(),
            tab: Tab::Home,
            config_path,
            settings: None,
            modes: None,
            models: None,
            voxtype: crate::supervisor::default_daemon_binary(),
            supervisor: Supervisor::disabled(),
            hud: None,
            #[cfg(any(
                target_os = "macos",
                windows,
                all(target_os = "linux", feature = "linux-tray")
            ))]
            tray: None,
            hide_on_close: false,
            quitting: false,
        }
    }

    pub fn with_voxtype(mut self, voxtype: PathBuf) -> Self {
        self.voxtype = voxtype;
        self
    }

    pub fn with_hud(mut self, hud: std::process::Child) -> Self {
        self.hud = Some(hud);
        self
    }

    pub fn with_supervisor(mut self, supervisor: Supervisor) -> Self {
        self.supervisor = supervisor;
        self
    }

    #[cfg(any(
        target_os = "macos",
        windows,
        all(target_os = "linux", feature = "linux-tray")
    ))]
    /// Show the tray. With `window_can_hide`, closing the window then hides
    /// it to the tray, and only the tray's Quit quits. winit can neither
    /// hide nor re-show a Wayland window, so there closing still quits.
    pub fn with_tray(
        mut self,
        tray: tray::Tray,
        actions: Receiver<tray::Action>,
        window_can_hide: bool,
    ) -> Self {
        self.tray = Some((tray, actions));
        self.hide_on_close = window_can_hide;
        self
    }

    /// Act on tray clicks and refresh the tray from the latest state.
    #[cfg(any(
        target_os = "macos",
        windows,
        all(target_os = "linux", feature = "linux-tray")
    ))]
    fn sync_tray(&mut self, ctx: &egui::Context) {
        let Some((tray, actions)) = &mut self.tray else {
            return;
        };
        let mut quit = false;
        while let Ok(action) = actions.try_recv() {
            match action {
                tray::Action::Toggle => match recording(&self.snap.daemon) {
                    Some(true) => {
                        let _ = self.requests.send(Request::Stop);
                    }
                    Some(false) => {
                        let _ = self.requests.send(Request::Start);
                    }
                    None => {}
                },
                tray::Action::Show => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
                tray::Action::RestartDaemon => self.supervisor.restart(),
                tray::Action::Quit => quit = true,
            }
        }
        tray.update(tray_view(&self.snap.daemon, &self.supervisor.status()));
        if quit {
            self.quit(ctx);
        }
    }

    /// Close the window and let the close through: the tray's Quit.
    #[cfg_attr(
        not(any(
            target_os = "macos",
            windows,
            all(target_os = "linux", feature = "linux-tray")
        )),
        allow(dead_code)
    )]
    fn quit(&mut self, ctx: &egui::Context) {
        self.quitting = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    /// A close request (the window's close button, Alt+F4, Cmd+W) hides the
    /// window when the tray is up; the tray's "Open Voxtype" shows it again.
    /// With no tray, or after the tray's Quit, the close goes through and
    /// the app exits.
    fn handle_close(&mut self, ctx: &egui::Context) {
        if !ctx.input(|i| i.viewport().close_requested()) || self.quitting || !self.hide_on_close {
            return;
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
    }

    fn send(&self, req: Request) {
        // The worker only stops when the app does.
        let _ = self.requests.send(req);
    }

    pub fn with_tab(mut self, tab: Tab) -> Self {
        self.tab = tab;
        self
    }

    /// Take the worker's latest snapshot, if any.
    fn drain(&mut self) {
        while let Ok(snap) = self.snapshots.try_recv() {
            self.snap = snap;
        }
    }

    /// Draw one frame. Separate from `eframe::App` so tests can run it on a
    /// bare `egui::Context`, with no window or GPU.
    pub fn show(&mut self, ui: &mut egui::Ui) {
        self.drain();
        #[cfg(any(
            target_os = "macos",
            windows,
            all(target_os = "linux", feature = "linux-tray")
        ))]
        self.sync_tray(&ui.ctx().clone());
        self.handle_close(&ui.ctx().clone());

        egui::Panel::top("tabs").show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.tab, Tab::Home, "Home");
                ui.selectable_value(&mut self.tab, Tab::Settings, "Settings");
                ui.selectable_value(&mut self.tab, Tab::Modes, "Modes");
                ui.selectable_value(&mut self.tab, Tab::Models, "Models");
            });
        });
        let Some(path) = self.config_path.clone() else {
            if self.tab != Tab::Home {
                egui::CentralPanel::default().show_inside(ui, |ui| {
                    ui.label("No config directory on this system; settings cannot be saved.");
                });
                return;
            }
            return self.home(ui);
        };
        match self.tab {
            Tab::Home => self.home(ui),
            Tab::Settings => self
                .settings
                .get_or_insert_with(|| Settings::new(path))
                .show(ui),
            Tab::Modes => {
                let modes = self.modes.get_or_insert_with(|| Modes::new(path));
                egui::CentralPanel::default().show_inside(ui, |ui| modes.show(ui));
            }
            Tab::Models => {
                let voxtype = self.voxtype.clone();
                let models = self.models.get_or_insert_with(|| {
                    Models::new(path, voxtype::config::Config::models_dir(), voxtype)
                });
                egui::CentralPanel::default().show_inside(ui, |ui| models.show(ui));
                // A newly selected model loads at the daemon's next start:
                // restart it now if this app owns it.
                if std::mem::take(&mut models.restart_wanted) {
                    let note = if self.supervisor.status().owned() {
                        self.supervisor.restart();
                        " Restarting the daemon to load it."
                    } else {
                        " Restart the daemon to load it."
                    };
                    if let Some(Ok(m)) = &mut models.message {
                        m.push_str(note);
                    }
                }
            }
        }
    }

    fn home(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("status").show_inside(ui, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let (label, color) = state_label(&self.snap.daemon);
                ui.colored_label(color, "\u{25CF}");
                ui.heading(label);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let rec = recording(&self.snap.daemon);
                    let stop = ui.add_enabled(rec == Some(true), egui::Button::new("Stop"));
                    if stop.clicked() {
                        self.send(Request::Stop);
                    }
                    let start = ui.add_enabled(rec == Some(false), egui::Button::new("Start"));
                    if start.clicked() {
                        self.send(Request::Start);
                    }
                });
            });
            let supervisor = self.supervisor.status();
            if let DaemonState::Unreachable(why) = &self.snap.daemon {
                ui.small(why.as_str());
                if matches!(supervisor, Status::Disabled | Status::External) {
                    ui.small("Start it with `voxtype daemon` (or your service manager).");
                }
            }
            ui.horizontal(|ui| {
                ui.small(supervisor.describe());
                if supervisor.owned() {
                    let label = if matches!(supervisor, Status::Running { .. }) {
                        "Restart daemon"
                    } else {
                        "Start daemon"
                    };
                    if ui.small_button(label).clicked() {
                        self.supervisor.restart();
                    }
                }
            });
            if let Some(err) = &self.snap.last_error {
                ui.colored_label(ui.visuals().error_fg_color, err.as_str());
            }
            ui.add_space(6.0);
        });

        egui::CentralPanel::default().show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading("History");
                if ui.small_button("Refresh").clicked() {
                    self.send(Request::RefreshHistory);
                }
            });
            ui.separator();
            match &self.snap.history {
                History::Disabled => {
                    ui.label(
                        "History is off. Set `[history] enabled = true` in config.toml \
                         to keep recent dictations (text only, never audio).",
                    );
                }
                History::Error(e) => {
                    ui.colored_label(ui.visuals().error_fg_color, e.as_str());
                }
                History::Entries(entries) if entries.is_empty() => {
                    ui.label("No dictations yet.");
                }
                History::Entries(entries) => {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            for e in entries {
                                ui.horizontal_wrapped(|ui| {
                                    if ui.small_button("Copy").clicked() {
                                        ui.ctx().copy_text(e.text.clone());
                                    }
                                    ui.weak(format_time(e.created_at_ms));
                                    ui.label(e.text.as_str());
                                });
                                ui.separator();
                            }
                        });
                }
            }
        });
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.show(ui);
        // A supervisor countdown or restart needs a frame even when the
        // daemon state itself is unchanged.
        if self.supervisor.status().owned() {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_secs(1));
        }
    }

    /// Quit (the tray's Quit, or closing the window when there is no tray):
    /// stop the daemon if this app started it.
    fn on_exit(&mut self) {
        if let Some(mut hud) = self.hud.take() {
            let _ = hud.kill();
            let _ = hud.wait();
        }
        self.supervisor.shutdown();
    }
}

/// `Some(recording?)` when start/stop applies, `None` when it doesn't.
fn recording(state: &DaemonState) -> Option<bool> {
    match state {
        DaemonState::Running(s) if s == "recording" => Some(true),
        DaemonState::Running(s) if s == "idle" => Some(false),
        _ => None,
    }
}

#[cfg(any(
    target_os = "macos",
    windows,
    all(target_os = "linux", feature = "linux-tray")
))]
fn tray_view(daemon: &DaemonState, supervisor: &Status) -> tray::View {
    let light = match daemon {
        DaemonState::Running(s) if s == "recording" => tray::Light::Recording,
        DaemonState::Running(s) if s == "transcribing" => tray::Light::Transcribing,
        DaemonState::Running(_) => tray::Light::Idle,
        _ => tray::Light::Down,
    };
    tray::View {
        light,
        status: format!("Voxtype: {}", state_label(daemon).0),
        supervisor: supervisor.describe(),
        recording: recording(daemon),
        can_restart: supervisor.owned(),
    }
}

fn state_label(state: &DaemonState) -> (String, egui::Color32) {
    match state {
        DaemonState::Unknown => ("Connecting...".into(), egui::Color32::GRAY),
        DaemonState::Unreachable(_) => ("Daemon not running".into(), egui::Color32::GRAY),
        DaemonState::Running(s) => {
            let color = match s.as_str() {
                "recording" => egui::Color32::from_rgb(0xd9, 0x3f, 0x3f),
                "transcribing" => egui::Color32::from_rgb(0xe0, 0xa0, 0x30),
                _ => egui::Color32::from_rgb(0x3f, 0xa8, 0x5a),
            };
            let mut label = s.clone();
            if let Some(first) = label.get_mut(0..1) {
                first.make_ascii_uppercase();
            }
            (label, color)
        }
    }
}

/// Local wall-clock time of a history entry.
fn format_time(ms: i64) -> String {
    use chrono::TimeZone;
    match chrono::Local.timestamp_millis_opt(ms) {
        chrono::LocalResult::Single(t) => t.format("%Y-%m-%d %H:%M").to_string(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use voxtype::history::Entry;

    fn frame(app: &mut App) {
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| app.show(ui));
    }

    #[test]
    fn renders_every_state_headless() {
        let (req_tx, _req_rx) = mpsc::channel();
        let (snap_tx, snap_rx) = mpsc::channel();
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::new(req_tx, snap_rx, Some(dir.path().join("config.toml")));
        frame(&mut app);
        let entry = Entry {
            id: 1,
            created_at_ms: 1_700_000_000_000,
            text: "hello".into(),
            engine: "whisper".into(),
            model: "base.en".into(),
        };
        for (daemon, history) in [
            (
                DaemonState::Unreachable("no socket".into()),
                History::Disabled,
            ),
            (
                DaemonState::Running("idle".into()),
                History::Entries(vec![]),
            ),
            (
                DaemonState::Running("recording".into()),
                History::Entries(vec![entry]),
            ),
            (
                DaemonState::Running("transcribing".into()),
                History::Error("locked".into()),
            ),
        ] {
            snap_tx
                .send(Snapshot {
                    daemon: daemon.clone(),
                    history,
                    last_error: Some("x".into()),
                })
                .unwrap();
            frame(&mut app);
            assert_eq!(app.snap.daemon, daemon);
        }
        for tab in [Tab::Settings, Tab::Modes, Tab::Models, Tab::Home] {
            app.tab = tab;
            frame(&mut app);
        }
        assert!(app.settings.is_some() && app.modes.is_some() && app.models.is_some());
    }

    #[test]
    fn start_stop_applies_only_when_idle_or_recording() {
        assert_eq!(recording(&DaemonState::Running("idle".into())), Some(false));
        assert_eq!(
            recording(&DaemonState::Running("recording".into())),
            Some(true)
        );
        assert_eq!(
            recording(&DaemonState::Running("transcribing".into())),
            None
        );
        assert_eq!(recording(&DaemonState::Unknown), None);
    }

    #[cfg(any(
        target_os = "macos",
        windows,
        all(target_os = "linux", feature = "linux-tray")
    ))]
    #[test]
    fn tray_view_follows_the_daemon() {
        let v = tray_view(
            &DaemonState::Running("recording".into()),
            &Status::Running {
                pid: 9,
                restarts: 0,
            },
        );
        assert_eq!(v.light, tray::Light::Recording);
        assert_eq!(v.status, "Voxtype: Recording");
        assert_eq!(v.recording, Some(true));
        assert!(v.can_restart);
        let v = tray_view(&DaemonState::Unreachable("x".into()), &Status::External);
        assert_eq!(v.light, tray::Light::Down);
        assert_eq!(v.recording, None);
        assert!(!v.can_restart);
    }

    /// One frame with a close request on the root viewport; returns the
    /// viewport commands the app sent.
    fn close_frame(app: &mut App) -> Vec<egui::ViewportCommand> {
        let ctx = egui::Context::default();
        let mut input = egui::RawInput::default();
        input.viewports.insert(
            egui::ViewportId::ROOT,
            egui::ViewportInfo {
                events: vec![egui::ViewportEvent::Close],
                ..Default::default()
            },
        );
        let out = ctx.run_ui(input, |ui| app.show(ui));
        out.viewport_output
            .get(&egui::ViewportId::ROOT)
            .map(|v| v.commands.clone())
            .unwrap_or_default()
    }

    fn bare_app() -> App {
        let (req_tx, _req_rx) = mpsc::channel();
        let (_snap_tx, snap_rx) = mpsc::channel();
        App::new(req_tx, snap_rx, None)
    }

    #[test]
    fn close_hides_to_tray_and_quit_still_quits() {
        use egui::ViewportCommand as C;
        // No tray (Linux without linux-tray, --no-tray, or the tray failed):
        // the close goes through and the app exits.
        let mut app = bare_app();
        let cmds = close_frame(&mut app);
        assert!(!cmds.contains(&C::CancelClose), "{cmds:?}");
        assert!(!cmds.contains(&C::Visible(false)), "{cmds:?}");

        // Tray up: the close is cancelled and the window hidden.
        let mut app = bare_app();
        app.hide_on_close = true;
        let cmds = close_frame(&mut app);
        assert!(cmds.contains(&C::CancelClose), "{cmds:?}");
        assert!(cmds.contains(&C::Visible(false)), "{cmds:?}");
        // A frame without a close request leaves the window alone.
        let ctx = egui::Context::default();
        let out = ctx.run_ui(egui::RawInput::default(), |ui| app.show(ui));
        let cmds = out
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .map(|v| v.commands.clone())
            .unwrap_or_default();
        assert!(!cmds.contains(&C::CancelClose), "{cmds:?}");

        // The tray's Quit closes the window, and that close goes through.
        let ctx = egui::Context::default();
        let out = ctx.run_ui(egui::RawInput::default(), |ui| app.quit(ui.ctx()));
        assert!(out.viewport_output[&egui::ViewportId::ROOT]
            .commands
            .contains(&C::Close));
        let cmds = close_frame(&mut app);
        assert!(!cmds.contains(&C::CancelClose), "{cmds:?}");
    }

    #[test]
    fn tab_names() {
        assert_eq!(Tab::parse("settings"), Some(Tab::Settings));
        assert_eq!(Tab::parse("modes"), Some(Tab::Modes));
        assert_eq!(Tab::parse("models"), Some(Tab::Models));
        assert_eq!(Tab::parse("home"), Some(Tab::Home));
        assert_eq!(Tab::parse("nope"), None);
    }

    #[test]
    fn labels() {
        assert_eq!(state_label(&DaemonState::Running("idle".into())).0, "Idle");
        assert_eq!(
            state_label(&DaemonState::Unreachable(String::new())).0,
            "Daemon not running"
        );
        assert_eq!(format_time(0).len(), "1970-01-01 00:00".len());
    }
}
