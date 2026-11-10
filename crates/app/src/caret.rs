//! The text caret's blink, run by the runtime instead of egui.
//!
//! egui blinks the caret of a focused text field by asking for a repaint at each phase change,
//! forever, and starts each timed frame one predicted frame early, so every phase change costs
//! several frames: a focused field kept an idle app drawing about 6 frames a second. The runtime
//! turns egui's blink off (egui then paints a solid caret and asks for nothing) and blinks the
//! caret itself: it hides the caret's stroke during the off phase and asks for exactly one frame
//! per phase change through the wake scheduler, credited to [`CARET`]. After [`BLINK_FOR`]
//! without input the caret stays solid and the app goes idle; the next input starts it again.

use egui::{Color32, Theme};

use crate::wake::{self, Reason};

/// Frames that only blink the text caret.
pub const CARET: Reason = Reason("caret");

/// Seconds of blinking after the last input; then the caret stays solid.
pub const BLINK_FOR: f64 = 10.0;

/// Blink state kept by the runtime between frames.
#[derive(Debug, Default)]
pub(crate) struct Caret {
    /// egui time of the last input event.
    last_input: Option<f64>,
    /// The stroke colors hidden for the off phase (dark, light), to put back.
    hidden: Option<[Color32; 2]>,
}

/// One blink cycle: visible for `on`, hidden for `off` seconds (egui's style values).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Cycle {
    on: f64,
    off: f64,
}

impl Cycle {
    fn of(ctx: &egui::Context) -> Cycle {
        let c = ctx.global_style().visuals.text_cursor.clone();
        let sane = |s: f32| if s.is_finite() { f64::from(s).clamp(0.05, 5.0) } else { 0.5 };
        Cycle { on: sane(c.on_duration), off: sane(c.off_duration) }
    }

    /// Whether the caret shows `elapsed` seconds after the last input, and the seconds until
    /// that changes (`None`: it stays as it is).
    fn phase(self, elapsed: f64) -> (bool, Option<f64>) {
        if !(0.0..BLINK_FOR).contains(&elapsed) {
            return (true, None);
        }
        let t = elapsed % (self.on + self.off);
        let (visible, change) = if t < self.on { (true, self.on - t) } else { (false, self.on + self.off - t) };
        let left = BLINK_FOR - elapsed;
        if change < left {
            (visible, Some(change))
        } else if visible {
            // blinking ends while visible: nothing left to draw
            (true, None)
        } else {
            // blinking ends while hidden: one frame to show the caret again
            (false, Some(left))
        }
    }
}

impl Caret {
    /// Before the frame draws: note input, keep egui's own blink off, and show or hide the
    /// caret for this frame's phase.
    pub(crate) fn begin(&mut self, ctx: &egui::Context) {
        let (now, input, window_focused) = ctx.input(|i| (i.time, i.events.iter().any(|e| !matches!(e, egui::Event::Screenshot { .. })), i.focused));
        if input || self.last_input.is_none() {
            self.last_input = Some(now);
        }
        if ctx.options(|o| o.dark_style.visuals.text_cursor.blink || o.light_style.visuals.text_cursor.blink) {
            ctx.all_styles_mut(|s| s.visuals.text_cursor.blink = false);
        }
        let blinking = window_focused && ctx.text_edit_focused();
        let visible = !blinking || self.phase(ctx, now).0;
        self.show(ctx, visible);
    }

    /// After the frame drew: ask for the frame of the next phase change, while a text field
    /// has the keyboard.
    pub(crate) fn end(&mut self, ctx: &egui::Context) {
        let (now, window_focused) = ctx.input(|i| (i.time, i.focused));
        if !(window_focused && ctx.text_edit_focused()) {
            // nothing to blink: the caret is solid wherever it shows next
            self.show(ctx, true);
            return;
        }
        if let (_, Some(delay)) = self.phase(ctx, now)
            && let Ok(delay) = std::time::Duration::try_from_secs_f64(delay)
        {
            wake::reschedule(ctx, CARET, delay);
        }
    }

    fn phase(&self, ctx: &egui::Context, now: f64) -> (bool, Option<f64>) {
        let elapsed = self.last_input.map_or(f64::INFINITY, |t| (now - t).max(0.0));
        Cycle::of(ctx).phase(elapsed)
    }

    fn show(&mut self, ctx: &egui::Context, visible: bool) {
        match (visible, self.hidden) {
            (true, Some([dark, light])) => {
                self.hidden = None;
                // put the colors back unless something restyled the caret meanwhile
                for (theme, color) in [(Theme::Dark, dark), (Theme::Light, light)] {
                    ctx.style_mut_of(theme, |s| {
                        if s.visuals.text_cursor.stroke.color == Color32::TRANSPARENT {
                            s.visuals.text_cursor.stroke.color = color;
                        }
                    });
                }
            }
            (false, None) => {
                let color = |t: Theme| ctx.style_of(t).visuals.text_cursor.stroke.color;
                self.hidden = Some([color(Theme::Dark), color(Theme::Light)]);
                ctx.all_styles_mut(|s| s.visuals.text_cursor.stroke.color = Color32::TRANSPARENT);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const C: Cycle = Cycle { on: 0.5, off: 0.5 };

    #[test]
    fn the_caret_blinks_then_stays_solid() {
        assert_eq!(C.phase(0.0), (true, Some(0.5)));
        assert_eq!(C.phase(0.75), (false, Some(0.25)));
        assert_eq!(C.phase(1.25), (true, Some(0.25)));
        // the last phase change before the end
        let (v, d) = C.phase(BLINK_FOR - 0.25);
        assert!(!v && d.is_some_and(|d| (d - 0.25).abs() < 1e-9));
        assert_eq!(C.phase(BLINK_FOR), (true, None));
        assert_eq!(C.phase(f64::INFINITY), (true, None));
        assert_eq!(C.phase(f64::NAN), (true, None));
        assert_eq!(C.phase(-1.0), (true, None));
    }

    #[test]
    fn phase_changes_over_the_blink_cost_one_frame_each() {
        let mut t = 0.0;
        let mut frames = 0;
        while let (_, Some(d)) = C.phase(t) {
            t += d;
            frames += 1;
            assert!(frames < 1000);
        }
        assert_eq!(frames, (BLINK_FOR / 0.5) as i32);
        assert!(t >= BLINK_FOR - 1e-9);
    }

    #[test]
    fn hiding_restores_the_theme_colors() {
        let ctx = egui::Context::default();
        let before = [Theme::Dark, Theme::Light].map(|t| ctx.style_of(t).visuals.text_cursor.stroke.color);
        let mut c = Caret::default();
        c.show(&ctx, false);
        assert_eq!(ctx.style_of(Theme::Dark).visuals.text_cursor.stroke.color, Color32::TRANSPARENT);
        c.show(&ctx, true);
        assert_eq!([Theme::Dark, Theme::Light].map(|t| ctx.style_of(t).visuals.text_cursor.stroke.color), before);
    }
}
