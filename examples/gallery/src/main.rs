//! `cargo run -p ashlar-example-gallery` — the component gallery.
//! `cargo run -p ashlar-example-gallery -- --snapshot <dir>` — render every page, light and dark, to PNG.

use ashlar_example_gallery::{Gallery, snapshot};

struct App(Gallery);

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.0.ui(ui);
    }
}

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(i) = args.iter().position(|a| a == "--snapshot") {
        let dir = args.get(i + 1).map_or_else(|| std::path::PathBuf::from("gallery-snapshots"), std::path::PathBuf::from);
        return match snapshot(&dir) {
            Ok(files) => {
                for f in files {
                    println!("{}", f.display());
                }
                std::process::ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("snapshot failed: {e}");
                std::process::ExitCode::FAILURE
            }
        };
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1180.0, 780.0]).with_title("ashlar gallery"),
        ..Default::default()
    };
    match eframe::run_native("ashlar gallery", options, Box::new(|_cc| Ok(Box::new(App(Gallery::new()))))) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("could not open the window: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}
