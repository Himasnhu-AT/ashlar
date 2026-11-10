//! The time frames see, for `app.clock`: pause, step and resume.
//!
//! The host (eframe or the headless [`crate::Driver`]) hands each frame its own time; at the start
//! of the frame ([`Clock::begin_frame`]) the clock rewrites `RawInput::time` and `predicted_dt`.
//! While paused every frame gets the frozen time and the `predicted_dt` the app had when it paused,
//! so whatever reads egui time stands still: egui's animations, `ashlar_ui::motion` (egui time plus
//! `predicted_dt`), tooltip delays, the caret's blink phase, toast timeouts, debounced work timed by
//! [`crate::wake`]. Input still runs frames (at the frozen time). After a resume the app's time
//! goes on from the frozen value, so nothing jumps; it then lags the host's clock by the time
//! spent paused (minus any steps).

/// Pause state and the offset between host time and app time.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct Clock {
    /// The host time of the latest frame.
    real: f64,
    /// Host time minus app time while running (time spent paused, minus steps).
    offset: f64,
    /// App time and `predicted_dt` while paused.
    frozen: Option<(f64, f32)>,
}

impl Clock {
    /// The start of a frame: rewrite its time while paused or after a pause.
    pub(crate) fn begin_frame(&mut self, raw: &mut egui::RawInput) {
        // a host that gives no time: egui goes on by `predicted_dt`
        let real = raw.time.unwrap_or(self.real + f64::from(raw.predicted_dt));
        self.real = real.max(self.real);
        match self.frozen {
            Some((t, dt)) => {
                raw.time = Some(t);
                raw.predicted_dt = dt;
            }
            None if self.offset != 0.0 => raw.time = Some(self.real - self.offset),
            None => {}
        }
    }

    /// The host time of the latest frame (for work that must not wait on a paused clock).
    pub(crate) fn host_time(&self) -> f64 {
        self.real
    }

    pub(crate) fn is_paused(&self) -> bool {
        self.frozen.is_some()
    }

    /// Freeze at `now` (the current frame's app time) unless already paused.
    pub(crate) fn pause(&mut self, now: f64, predicted_dt: f32) {
        if self.frozen.is_none() {
            let dt = if predicted_dt.is_finite() && predicted_dt > 0.0 { predicted_dt } else { 1.0 / 60.0 };
            self.frozen = Some((now, dt));
        }
    }

    /// Pause if running, then move the frozen time on by `secs`: the next frame sees it.
    pub(crate) fn step(&mut self, now: f64, predicted_dt: f32, secs: f64) {
        self.pause(now, predicted_dt);
        if let Some((t, _)) = &mut self.frozen {
            *t += secs.max(0.0);
        }
    }

    /// Run again from the frozen time: the next frame is later by however much host time passes.
    pub(crate) fn resume(&mut self) {
        if let Some((t, _)) = self.frozen.take() {
            self.offset = self.real - t;
        }
    }

    /// The app time the next frame sees, when known (paused), else `now`.
    pub(crate) fn time(&self, now: f64) -> f64 {
        self.frozen.map_or(now, |(t, _)| t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(c: &mut Clock, host: f64) -> f64 {
        let mut raw = egui::RawInput { time: Some(host), predicted_dt: 1.0 / 60.0, ..Default::default() };
        c.begin_frame(&mut raw);
        raw.time.unwrap_or(f64::NAN)
    }

    #[test]
    fn pause_step_resume() {
        let mut c = Clock::default();
        assert_eq!(frame(&mut c, 1.0), 1.0, "a clock never paused changes nothing");
        c.pause(1.0, 1.0 / 60.0);
        assert_eq!(frame(&mut c, 2.0), 1.0);
        assert_eq!(frame(&mut c, 3.0), 1.0);
        c.step(1.0, 1.0 / 60.0, 0.25);
        assert_eq!(frame(&mut c, 3.5), 1.25);
        assert_eq!(frame(&mut c, 4.0), 1.25, "a step is one frame's worth");
        c.resume();
        assert!(!c.is_paused());
        assert_eq!(frame(&mut c, 4.5), 1.75, "time goes on from the frozen value");
        assert_eq!(c.host_time(), 4.5);
    }

    #[test]
    fn host_time_never_goes_back() {
        let mut c = Clock::default();
        frame(&mut c, 5.0);
        frame(&mut c, 4.0);
        assert_eq!(c.host_time(), 5.0);
    }
}
