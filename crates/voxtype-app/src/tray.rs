//! System tray icon (`tray-icon`): the daemon's state at a glance, plus
//! start/stop, show window, restart daemon and quit.
//!
//! This generalizes the macOS-only `voxtype menubar` (`src/menubar.rs`) to
//! macOS, Windows and Linux. On macOS and Windows the icon lives on the
//! main thread inside eframe's event loop. On Linux it needs GTK and a
//! StatusNotifier host (Waybar, KDE, GNOME with the AppIndicator
//! extension), so it is behind the `linux-tray` feature and runs its own
//! GTK thread; Waybar's `voxtype status` module stays the main Linux path.

use std::sync::mpsc::Sender;

use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

/// What a tray menu click asks the app to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Toggle,
    Show,
    RestartDaemon,
    Quit,
}

const TOGGLE: &str = "toggle";
const SHOW: &str = "show";
const RESTART: &str = "restart";
const QUIT: &str = "quit";

impl Action {
    fn from_id(id: &MenuId) -> Option<Self> {
        match id.as_ref() {
            TOGGLE => Some(Action::Toggle),
            SHOW => Some(Action::Show),
            RESTART => Some(Action::RestartDaemon),
            QUIT => Some(Action::Quit),
            _ => None,
        }
    }
}

/// The icon's colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Light {
    Idle,
    Recording,
    Transcribing,
    Down,
}

/// Everything the tray shows. The app rebuilds it each frame; the tray is
/// only touched when it changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct View {
    pub light: Light,
    /// "Voxtype: Idle", ...
    pub status: String,
    /// The supervisor line.
    pub supervisor: String,
    /// `Some(true)` = "Stop recording", `Some(false)` = "Start recording",
    /// `None` = disabled (daemon down or busy).
    pub recording: Option<bool>,
    /// Restart applies only to a daemon this app started.
    pub can_restart: bool,
}

/// The tray. Dropping it removes the icon.
pub struct Tray {
    #[cfg(not(target_os = "linux"))]
    widgets: Widgets,
    #[cfg(target_os = "linux")]
    updates: Sender<View>,
    last: Option<View>,
}

impl Tray {
    /// Create the icon. Menu clicks go to `actions`; `wake` runs after each
    /// (the app passes `request_repaint`).
    pub fn new(
        actions: Sender<Action>,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Result<Self, String> {
        let actions = std::sync::Mutex::new(actions);
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            if let Some(action) = Action::from_id(&event.id) {
                let _ = actions
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .send(action);
                wake();
            }
        }));
        Self::platform_new()
    }

    #[cfg(not(target_os = "linux"))]
    fn platform_new() -> Result<Self, String> {
        Ok(Self {
            widgets: Widgets::build()?,
            last: None,
        })
    }

    /// GTK has to own the icon: init it on a thread of its own and run its
    /// main loop there, polling for view updates.
    #[cfg(target_os = "linux")]
    fn platform_new() -> Result<Self, String> {
        use gtk::glib;
        use std::sync::mpsc;
        use std::time::Duration;

        let (updates, rx) = mpsc::channel::<View>();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
        std::thread::Builder::new()
            .name("voxtype-app-tray".into())
            .spawn(move || {
                if let Err(e) = gtk::init() {
                    let _ = ready_tx.send(Err(format!("GTK init failed: {e}")));
                    return;
                }
                // libappindicator is loaded at runtime and panics when the
                // library is missing; report that as "no tray".
                let built = std::panic::catch_unwind(Widgets::build)
                    .unwrap_or_else(|_| Err("libayatana-appindicator3 not available".into()));
                let mut widgets = match built {
                    Ok(w) => w,
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                        return;
                    }
                };
                let _ = ready_tx.send(Ok(()));
                glib::timeout_add_local(Duration::from_millis(200), move || loop {
                    match rx.try_recv() {
                        Ok(view) => widgets.apply(&view),
                        Err(mpsc::TryRecvError::Empty) => return glib::ControlFlow::Continue,
                        Err(mpsc::TryRecvError::Disconnected) => {
                            gtk::main_quit();
                            return glib::ControlFlow::Break;
                        }
                    }
                });
                gtk::main();
            })
            .map_err(|e| format!("tray thread: {e}"))?;
        ready_rx
            .recv()
            .map_err(|_| "tray thread exited".to_string())??;
        Ok(Self {
            updates,
            last: None,
        })
    }

    /// Show `view`, if it differs from what is shown.
    pub fn update(&mut self, view: View) {
        if self.last.as_ref() == Some(&view) {
            return;
        }
        #[cfg(not(target_os = "linux"))]
        self.widgets.apply(&view);
        #[cfg(target_os = "linux")]
        let _ = self.updates.send(view.clone());
        self.last = Some(view);
    }
}

struct Widgets {
    icon: TrayIcon,
    status: MenuItem,
    supervisor: MenuItem,
    toggle: MenuItem,
    restart: MenuItem,
    light: Light,
}

impl Widgets {
    fn build() -> Result<Self, String> {
        let status = MenuItem::new("Voxtype", false, None);
        let supervisor = MenuItem::new("", false, None);
        let toggle = MenuItem::with_id(TOGGLE, "Start recording", false, None);
        let show = MenuItem::with_id(SHOW, "Open Voxtype", true, None);
        let restart = MenuItem::with_id(RESTART, "Restart daemon", false, None);
        let quit = MenuItem::with_id(QUIT, "Quit", true, None);
        let menu = Menu::new();
        menu.append_items(&[
            &status,
            &supervisor,
            &PredefinedMenuItem::separator(),
            &toggle,
            &show,
            &restart,
            &PredefinedMenuItem::separator(),
            &quit,
        ])
        .map_err(|e| format!("tray menu: {e}"))?;
        let icon = TrayIconBuilder::new()
            .with_id("voxtype-app")
            .with_menu(Box::new(menu))
            .with_tooltip("Voxtype")
            .with_icon(icon(Light::Down))
            .build()
            .map_err(|e| format!("tray icon: {e}"))?;
        Ok(Self {
            icon,
            status,
            supervisor,
            toggle,
            restart,
            light: Light::Down,
        })
    }

    fn apply(&mut self, view: &View) {
        if view.light != self.light {
            let _ = self.icon.set_icon(Some(icon(view.light)));
            self.light = view.light;
        }
        let _ = self
            .icon
            .set_tooltip(Some(format!("{}\n{}", view.status, view.supervisor)));
        self.status.set_text(&view.status);
        self.supervisor.set_text(&view.supervisor);
        match view.recording {
            Some(true) => {
                self.toggle.set_text("Stop recording");
                self.toggle.set_enabled(true);
            }
            Some(false) => {
                self.toggle.set_text("Start recording");
                self.toggle.set_enabled(true);
            }
            None => {
                self.toggle.set_text("Start recording");
                self.toggle.set_enabled(false);
            }
        }
        self.restart.set_enabled(view.can_restart);
    }
}

/// A filled circle in the state's colour, drawn at runtime so the crate
/// ships no image assets.
fn icon(light: Light) -> Icon {
    const SIZE: u32 = 32;
    let (rgba, size) = (circle(light_rgb(light), SIZE), SIZE);
    Icon::from_rgba(rgba, size, size).expect("a 32x32 RGBA buffer is a valid icon")
}

pub fn light_rgb(light: Light) -> [u8; 3] {
    match light {
        Light::Idle => [0x3f, 0xa8, 0x5a],
        Light::Recording => [0xd9, 0x3f, 0x3f],
        Light::Transcribing => [0xe0, 0xa0, 0x30],
        Light::Down => [0x80, 0x80, 0x80],
    }
}

/// An anti-aliased disc, `size` x `size`, RGBA.
fn circle([r, g, b]: [u8; 3], size: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity((size * size * 4) as usize);
    let c = size as f32 / 2.0;
    let radius = c - 2.0;
    for y in 0..size {
        for x in 0..size {
            let d = ((x as f32 + 0.5 - c).powi(2) + (y as f32 + 0.5 - c).powi(2)).sqrt();
            let alpha = (radius + 0.5 - d).clamp(0.0, 1.0);
            out.extend_from_slice(&[r, g, b, (alpha * 255.0) as u8]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_ids_map_to_actions() {
        for (id, action) in [
            (TOGGLE, Action::Toggle),
            (SHOW, Action::Show),
            (RESTART, Action::RestartDaemon),
            (QUIT, Action::Quit),
        ] {
            assert_eq!(Action::from_id(&MenuId::new(id)), Some(action));
        }
        assert_eq!(Action::from_id(&MenuId::new("nope")), None);
    }

    #[test]
    fn icons_are_round() {
        let px = circle([1, 2, 3], 32);
        assert_eq!(px.len(), 32 * 32 * 4);
        let alpha = |x: usize, y: usize| px[(y * 32 + x) * 4 + 3];
        assert_eq!(alpha(16, 16), 255);
        assert_eq!(alpha(0, 0), 0);
        for light in [
            Light::Idle,
            Light::Recording,
            Light::Transcribing,
            Light::Down,
        ] {
            let _ = icon(light);
        }
    }
}
