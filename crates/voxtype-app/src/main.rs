//! `voxtype-app`: the desktop client for the voxtype daemon (M6, egui).
//!
//! The daemon stays headless. This app is one more client of the control
//! protocol v1 (`status`, `start`, `stop`) and reads the opt-in history
//! database. A crash here never loses a recording. It starts the daemon
//! when none is running and supervises the one it started
//! (`supervisor.rs`), and shows a tray icon (`tray.rs`). See
//! `docs/GUI-STACK-ADR.md` for the stack and `docs/HEX-ABSORB-PLAN.md` M6.

// No console window behind the app on Windows release builds.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod client;
mod models;
mod modes;
mod settings;
mod supervisor;
#[cfg(any(
    target_os = "macos",
    windows,
    all(target_os = "linux", feature = "linux-tray")
))]
mod tray;
mod ui;

use eframe::egui;

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("voxtype-app {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!(
            "voxtype-app {}: desktop app for the voxtype daemon\n\n\
             Usage: voxtype-app [OPTIONS]\n\n\
             --tab NAME         open on this tab: home, settings, modes or models\n\
             --no-tray          no tray icon\n\
             --no-supervise     never start, restart or stop the daemon\n\
             --daemon-bin PATH  the voxtype binary for the daemon and downloads\n\
             \x20                  (default: next to this app, else PATH)\n\
             -V, --version\n\
             -h, --help",
            env!("CARGO_PKG_VERSION")
        );
        return Ok(());
    }
    let tab = match args.iter().position(|a| a == "--tab") {
        None => ui::Tab::Home,
        Some(i) => match args.get(i + 1).and_then(|t| ui::Tab::parse(t)) {
            Some(tab) => tab,
            None => {
                eprintln!("voxtype-app: --tab takes home, settings, modes or models");
                std::process::exit(2);
            }
        },
    };
    let no_tray = args.iter().any(|a| a == "--no-tray");
    let supervise = !args.iter().any(|a| a == "--no-supervise");
    let daemon_bin = match args.iter().position(|a| a == "--daemon-bin") {
        None => None,
        Some(i) => match args.get(i + 1) {
            Some(path) => Some(std::path::PathBuf::from(path)),
            None => {
                eprintln!("voxtype-app: --daemon-bin takes a path");
                std::process::exit(2);
            }
        },
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Voxtype")
            .with_app_id("voxtype-app")
            .with_inner_size([420.0, 520.0])
            .with_min_inner_size([320.0, 240.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Voxtype",
        options,
        Box::new(|cc| {
            let ctx = cc.egui_ctx.clone();
            let worker =
                client::Worker::spawn(client::Local::from_config(), move || ctx.request_repaint());
            let voxtype_bin = daemon_bin
                .clone()
                .unwrap_or_else(supervisor::default_daemon_binary);
            let supervisor = if supervise {
                let ctx = cc.egui_ctx.clone();
                supervisor::Supervisor::spawn(
                    supervisor::Launch::daemon(daemon_bin),
                    supervisor::Backoff::default(),
                    supervisor::daemon_running,
                    move || ctx.request_repaint(),
                )
            } else {
                supervisor::Supervisor::disabled()
            };
            #[allow(unused_mut)]
            let mut app = ui::App::new(
                worker.requests,
                worker.snapshots,
                voxtype::config::Config::default_path(),
            )
            .with_tab(tab)
            .with_voxtype(voxtype_bin)
            .with_supervisor(supervisor);
            #[cfg(any(
                target_os = "macos",
                windows,
                all(target_os = "linux", feature = "linux-tray")
            ))]
            if !no_tray {
                let (tx, rx) = std::sync::mpsc::channel();
                let ctx = cc.egui_ctx.clone();
                match tray::Tray::new(tx, move || ctx.request_repaint()) {
                    Ok(t) => app = app.with_tray(t, rx),
                    Err(e) => eprintln!("voxtype-app: no tray icon ({e})"),
                }
            }
            #[cfg(not(any(
                target_os = "macos",
                windows,
                all(target_os = "linux", feature = "linux-tray")
            )))]
            let _ = no_tray;
            Ok(Box::new(app))
        }),
    )
}
