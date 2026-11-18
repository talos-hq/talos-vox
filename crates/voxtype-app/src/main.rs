//! `voxtype-app`: the desktop client for the voxtype daemon (M6, egui).
//!
//! The daemon stays headless. This app is one more client of the control
//! protocol v1 (`status`, `start`, `stop`) and reads the opt-in history
//! database. A crash here never loses a recording. See
//! `docs/GUI-STACK-ADR.md` for the stack and `docs/HEX-ABSORB-PLAN.md` M6
//! for what comes next (tray, settings, model picker, HUD).

// No console window behind the app on Windows release builds.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod client;
mod modes;
mod settings;
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
             Usage: voxtype-app [--tab home|settings|modes]\n\n\
             --tab NAME   open on this tab (default: home)\n\
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
                eprintln!("voxtype-app: --tab takes home, settings or modes");
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
            Ok(Box::new(
                ui::App::new(
                    worker.requests,
                    worker.snapshots,
                    voxtype::config::Config::default_path(),
                )
                .with_tab(tab),
            ))
        }),
    )
}
