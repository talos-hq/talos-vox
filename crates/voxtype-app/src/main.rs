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
mod ui;

use eframe::egui;

fn main() -> eframe::Result {
    if std::env::args().any(|a| a == "--version" || a == "-V") {
        println!("voxtype-app {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
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
            Ok(Box::new(ui::App::new(worker.requests, worker.snapshots)))
        }),
    )
}
