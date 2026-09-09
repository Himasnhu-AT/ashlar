//! Telemetry for an egui app (feature `telemetry`, over `ashlar-telemetry`): the per-frame probe
//! and a read-only privacy section whose words come from the app.

use std::time::Instant;

use ashlar_telemetry::{Telemetry, TurnGuard};
use egui::{Rect, RichText, Ui};

use crate::automation::{self, Details, Role};

/// What the frame loop calls, once per pass.
pub struct FrameProbe;

impl FrameProbe {
    /// At the top of the logic pass: notes focus and whether the user did anything, and opens a
    /// hang-watchdog turn that ends when the guard is dropped (keep it for the whole pass).
    pub fn logic(telemetry: &Telemetry, ctx: &egui::Context) -> TurnGuard {
        let turn = telemetry.turn();
        let (focused, active) = ctx.input(|i| (i.focused, !i.events.is_empty()));
        telemetry.input(focused, active);
        turn
    }

    /// Around the UI pass: a watchdog turn and the pass's time as one frame. `counts` is false
    /// for passes that only set things up (fonts, theme) and draw nothing real; the first counted
    /// frame also records the startup time.
    pub fn ui<R>(telemetry: &Telemetry, counts: bool, pass: impl FnOnce() -> R) -> R {
        let _turn = telemetry.turn();
        let started = Instant::now();
        let r = pass();
        if counts {
            telemetry.frame(started.elapsed());
        }
        r
    }
}

/// The words of a read-only privacy section, all from the app.
#[derive(Clone, Copy, Debug)]
pub struct PrivacyText<'a> {
    /// Where reports go, or why nothing is sent (see `ashlar_telemetry::Telemetry::status`).
    pub status: &'a str,
    pub sent_heading: &'a str,
    /// One line per kind of data.
    pub sent: &'a [&'a str],
    pub never_sent: &'a str,
    /// A last, quieter line (for example that it cannot be switched off).
    pub footer: &'a str,
}

/// Draw a read-only privacy section. Automation ids `<id>.status`, `<id>.sent.<n>`,
/// `<id>.neverSent`, `<id>.footer`; the same ids and rects are returned for apps that keep their
/// own automation registry.
pub fn privacy_section(ui: &mut Ui, id: &str, text: &PrivacyText<'_>) -> Vec<(String, Rect)> {
    let mut out = Vec::new();
    let mut line = |ui: &mut Ui, sub: String, s: &str, rich: RichText| {
        let r = ui.add(egui::Label::new(rich).wrap());
        automation::register_with(ui, &r, &sub, s, Role::Status, Details { value: Some(s), selected: None });
        out.push((sub, r.rect));
    };
    line(ui, format!("{id}.status"), text.status, RichText::new(text.status));
    ui.add_space(6.0);
    ui.label(RichText::new(text.sent_heading).strong());
    for (i, s) in text.sent.iter().enumerate() {
        line(ui, format!("{id}.sent.{i}"), s, RichText::new(format!("•  {s}")));
    }
    ui.add_space(6.0);
    line(ui, format!("{id}.neverSent"), text.never_sent, RichText::new(text.never_sent));
    if !text.footer.is_empty() {
        ui.add_space(6.0);
        let weak = RichText::new(text.footer).weak();
        line(ui, format!("{id}.footer"), text.footer, weak);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_section_shows_the_apps_words_with_ids() {
        let ctx = egui::Context::default();
        crate::automation::install(&ctx);
        let text = PrivacyText {
            status: "Reports go to t.example.com.",
            sent_heading: "What is sent",
            sent: &["Crashes", "Frame times"],
            never_sent: "Never: files.",
            footer: "Always on.",
        };
        let mut got = Vec::new();
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| got = privacy_section(ui, "privacy", &text));
        out.textures_delta.clear();
        let ids: Vec<&str> = got.iter().map(|(i, _)| i.as_str()).collect();
        assert_eq!(ids, ["privacy.status", "privacy.sent.0", "privacy.sent.1", "privacy.neverSent", "privacy.footer"]);
    }

    #[test]
    fn the_probe_runs_with_a_disabled_client() {
        let ctx = egui::Context::default();
        let t = Telemetry::default();
        let turn = FrameProbe::logic(&t, &ctx);
        drop(turn);
        assert_eq!(FrameProbe::ui(&t, true, || 3), 3);
    }
}
