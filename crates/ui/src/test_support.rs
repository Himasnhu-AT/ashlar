//! Headless frame helpers for unit tests.

/// Runs one frame and discards its texture uploads. There is no renderer in a unit test, and epaint
/// 0.36 asserts (in debug builds) that a dropped `TexturesDelta` was handled.
pub(crate) fn run(ctx: &egui::Context, input: egui::RawInput, ui: impl FnMut(&mut egui::Ui)) -> egui::FullOutput {
    let mut output = ctx.run_ui(input, ui);
    output.textures_delta.clear();
    output
}

/// Press and release the primary button at `p`, one frame per event.
pub(crate) fn click<S>(h: &mut egui_kittest::Harness<'_, S>, p: egui::Pos2) {
    h.hover_at(p);
    h.step();
    h.drag_at(p);
    h.step();
    h.drop_at(p);
    h.step();
}
