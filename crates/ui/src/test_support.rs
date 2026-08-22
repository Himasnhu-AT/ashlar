//! Headless frame helpers for unit tests.

/// Runs one frame and discards its texture uploads. There is no renderer in a unit test, and epaint
/// 0.36 asserts (in debug builds) that a dropped `TexturesDelta` was handled.
pub(crate) fn run(ctx: &egui::Context, input: egui::RawInput, ui: impl FnMut(&mut egui::Ui)) -> egui::FullOutput {
    let mut output = ctx.run_ui(input, ui);
    output.textures_delta.clear();
    output
}
