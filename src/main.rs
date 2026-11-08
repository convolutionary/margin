#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
//! Margin: a text editor for files of any size.

mod app;
mod chrome;
mod editor;
mod ipc;
mod session;
mod syntax;
mod theme;

use std::path::PathBuf;

const HELP: &str = "Margin, a text editor for files of any size

usage: margin [options] [file[:line]]...

  -n, --new-window   don't hand the files to a Margin that's already open
  -h, --help         this
  -V, --version      the version
";

fn main() -> eframe::Result {
    let mut files = Vec::new();
    let mut new_window = false;
    for a in std::env::args_os().skip(1) {
        match a.to_str() {
            Some("-h") | Some("--help") => {
                print!("{HELP}");
                return Ok(());
            }
            Some("-V") | Some("--version") => {
                println!("margin {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            Some("-n") | Some("--new-window") => new_window = true,
            _ => {
                let p = PathBuf::from(&a);
                // keep "file:12" as written; make the rest absolute for the window that gets them
                files.push(if p.is_relative() { std::env::current_dir().map(|d| d.join(&p)).unwrap_or(p) } else { p });
            }
        }
    }
    if !new_window && ipc::hand_over(&files) {
        return Ok(());
    }
    let (tx, rx) = crossbeam_channel::unbounded();
    let ctx_slot: std::sync::Arc<std::sync::OnceLock<egui::Context>> = Default::default();
    {
        let slot = ctx_slot.clone();
        ipc::listen(move |paths| {
            let _ = tx.send(paths);
            if let Some(c) = slot.get() {
                c.request_repaint();
            }
        });
    }

    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon-256.png")).ok();
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Margin")
        .with_app_id("margin")
        .with_inner_size([1180.0, 780.0])
        .with_min_inner_size([420.0, 260.0])
        .with_drag_and_drop(true);
    if let Some(i) = icon {
        viewport = viewport.with_icon(i);
    }
    let options = eframe::NativeOptions { viewport, ..Default::default() };
    eframe::run_native(
        "margin",
        options,
        Box::new(move |cc| {
            let _ = ctx_slot.set(cc.egui_ctx.clone());
            Ok(Box::new(Margin(app::App::new(cc, files, rx))))
        }),
    )
}

struct Margin(app::App);

impl eframe::App for Margin {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if ctx.input(|i| i.viewport().close_requested()) {
            self.0.on_close_request(ctx);
        }
        self.0.tick(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.0.shortcuts(&ctx);
        self.0.draw(ui);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.0.save_session();
    }
}
