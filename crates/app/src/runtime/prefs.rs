//! The OS preferences a running app follows: reduced motion, the accent colour and increased
//! contrast. Read once at startup and again whenever the window gains focus (no polling). On
//! Linux the read is a D-Bus round trip, so it runs on a background thread and lands a frame or
//! two later; elsewhere it is cheap and runs in place.

use ashlar_motion::MotionPrefs;
use ashlar_native::system::Preferences;
use ashlar_ui::motion;
use ashlar_ui::tokens::{self, Tokens};
use egui::{Color32, Theme};

/// Where preferences come from: the OS, or a test's stand-in.
pub(crate) type PrefsSource = Box<dyn FnMut() -> Preferences>;

pub(crate) struct OsPrefs {
    /// `None` reads the OS.
    pub(crate) source: Option<PrefsSource>,
    /// Recolour the accent from the OS (`App::follow_system_accent`).
    follow_accent: bool,
    /// Motion settings that win over the OS (`App::motion`).
    motion: Option<MotionPrefs>,
    /// The token sets installed at startup.
    installed: Option<(Tokens, Tokens)>,
    /// The app installed tokens of its own: leave them alone.
    app_tokens: bool,
    applied: Option<Preferences>,
    #[cfg(target_os = "linux")]
    pending: Option<std::sync::mpsc::Receiver<Preferences>>,
    focused: bool,
}

impl OsPrefs {
    pub(crate) fn new(follow_accent: bool, motion: Option<MotionPrefs>) -> Self {
        OsPrefs {
            source: None,
            follow_accent,
            motion,
            installed: None,
            app_tokens: false,
            applied: None,
            #[cfg(target_os = "linux")]
            pending: None,
            focused: true,
        }
    }
}

impl OsPrefs {
    /// Read the preferences now, or (Linux, OS source) start a background read and return `None`.
    fn read(&mut self, ctx: &egui::Context) -> Option<Preferences> {
        if let Some(source) = &mut self.source {
            return Some(source());
        }
        #[cfg(target_os = "linux")]
        {
            if self.pending.is_none() {
                let (tx, rx) = std::sync::mpsc::channel();
                let waker = ashlar_ui::wake::waker(ctx, ashlar_ui::wake::Reason::SETTINGS);
                let spawned = std::thread::Builder::new().name("ashlar-os-prefs".into()).spawn(move || {
                    let p = std::panic::catch_unwind(ashlar_native::system::preferences).unwrap_or_default();
                    if tx.send(p).is_ok() {
                        waker.wake();
                    }
                });
                match spawned {
                    Ok(_) => self.pending = Some(rx),
                    Err(e) => log::warn!("could not read the system preferences: {e}"),
                }
            }
            None
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = ctx;
            Some(ashlar_native::system::preferences())
        }
    }

    /// The light and dark tokens for `p`, from the built-in sets.
    fn derive(&self, p: &Preferences) -> (Tokens, Tokens) {
        let accent = p.accent.filter(|_| self.follow_accent).map(|[r, g, b]| Color32::from_rgb(r, g, b));
        let contrast = p.increased_contrast == Some(true);
        let f = |t: Tokens| {
            let t = accent.map_or(t, |a| t.with_accent(a));
            if contrast { t.high_contrast() } else { t }
        };
        (f(Tokens::light()), f(Tokens::dark()))
    }

    fn apply_motion(&self, ctx: &egui::Context, p: &Preferences) {
        let want = self.motion.unwrap_or(MotionPrefs::new(p.reduced_motion.unwrap_or(false), 1.0));
        if motion::prefs(ctx) != want {
            motion::set_prefs(ctx, want);
        }
    }

    /// At startup, before the app's setup runs: the token sets to install (OS accent and
    /// contrast applied when known), with the motion settings already set.
    pub(crate) fn startup(&mut self, ctx: &egui::Context) -> (Tokens, Tokens) {
        let p = self.read(ctx);
        self.apply_motion(ctx, &p.unwrap_or_default());
        let sets = p.map_or((Tokens::light(), Tokens::dark()), |p| self.derive(&p));
        self.applied = p;
        self.installed = Some(sets);
        sets
    }

    /// Each frame: re-read when the window gains focus, and pick up a background read.
    pub(crate) fn poll(&mut self, ctx: &egui::Context) {
        let (focused, event) = ctx.input(|i| (i.focused, i.events.iter().any(|e| matches!(e, egui::Event::WindowFocused(true)))));
        if (event || (focused && !self.focused))
            && let Some(p) = self.read(ctx)
        {
            self.apply(ctx, p);
        }
        self.focused = focused;
        #[cfg(target_os = "linux")]
        if let Some(rx) = &self.pending {
            match rx.try_recv() {
                Ok(p) => {
                    self.pending = None;
                    self.apply(ctx, p);
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.pending = None,
            }
        }
    }

    /// Follow `p` if it differs from what was applied last.
    fn apply(&mut self, ctx: &egui::Context, p: Preferences) {
        if self.applied == Some(p) {
            return;
        }
        self.apply_motion(ctx, &p);
        // tokens the app installed itself (in its setup or later) win over the OS
        if let Some((light, dark)) = self.installed
            && (tokens::of(ctx, Theme::Light) != light || tokens::of(ctx, Theme::Dark) != dark)
        {
            self.app_tokens = true;
        }
        if !self.app_tokens {
            let (light, dark) = self.derive(&p);
            tokens::apply_colors(ctx, &light);
            tokens::apply_colors(ctx, &dark);
            self.installed = Some((light, dark));
        }
        self.applied = Some(p);
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use ashlar_ui::wake::{Reason, Scheduler};

    use super::*;
    use crate::{App, Driver, Runtime};

    const ACCENT: [u8; 3] = [200, 40, 120];

    fn app() -> App<i64> {
        App::<i64>::new("prefstest", "0.0.0")
    }

    fn driver(rt: Runtime<i64>) -> Driver<i64> {
        let mut d = Driver::new(rt);
        d.frame();
        d
    }

    fn with(app: App<i64>, p: Preferences) -> Driver<i64> {
        let mut rt = app.runtime();
        rt.set_preferences(move || p);
        driver(rt)
    }

    #[test]
    fn reduced_motion_zeroes_animation_time_and_stops_the_sweep() {
        let mut d = with(
            app().view(|ui, _| drop(ashlar_ui::status_bar::progress(ui, "busy", "Working", None))),
            Preferences { reduced_motion: Some(true), ..Default::default() },
        );
        assert!(motion::prefs(d.ctx()).reduced);
        assert_eq!(d.ctx().global_style().animation_time, 0.0);
        let before = Scheduler::of(d.ctx()).stats().requests_for(Reason::ANIMATION);
        for _ in 0..3 {
            d.frame();
        }
        assert_eq!(Scheduler::of(d.ctx()).stats().requests_for(Reason::ANIMATION), before, "the sweep holds still");
    }

    #[test]
    fn os_accent_and_contrast_reach_the_tokens() {
        let d = with(app(), Preferences { accent: Some(ACCENT), increased_contrast: Some(true), ..Default::default() });
        let t = tokens::current(d.ctx());
        assert_eq!(t.accent, Color32::from_rgb(200, 40, 120));
        let want = if t.dark { Tokens::dark() } else { Tokens::light() };
        assert_eq!(t, want.with_accent(t.accent).high_contrast());
    }

    #[test]
    fn app_pins_win_over_the_os() {
        let p = Preferences { accent: Some(ACCENT), reduced_motion: Some(true), ..Default::default() };
        let d = with(app().follow_system_accent(false).motion(MotionPrefs::new(false, 0.5)), p);
        assert_eq!(tokens::current(d.ctx()).accent, Tokens::for_theme(d.ctx().theme()).accent);
        assert_eq!(motion::prefs(d.ctx()), MotionPrefs::new(false, 0.5));
        // tokens installed by the app's setup are kept
        let mine = Tokens::dark().with_accent(Color32::from_rgb(10, 160, 90));
        let d = with(app().setup(move |ctx| tokens::install(ctx, &Tokens::light(), &mine, tokens::ThemeMode::Dark)), p);
        assert_eq!(tokens::current(d.ctx()).accent, Color32::from_rgb(10, 160, 90));
    }

    #[test]
    fn preferences_are_read_again_when_the_window_gains_focus() {
        let reads = Rc::new(Cell::new(0));
        let reduced = Rc::new(Cell::new(false));
        let mut rt = app().runtime();
        let (r, red) = (Rc::clone(&reads), Rc::clone(&reduced));
        rt.set_preferences(move || {
            r.set(r.get() + 1);
            Preferences { reduced_motion: Some(red.get()), accent: red.get().then_some(ACCENT), ..Default::default() }
        });
        let mut d = driver(rt);
        d.frame();
        assert_eq!(reads.get(), 1, "read once at startup, not per frame");
        assert!(!motion::prefs(d.ctx()).reduced);
        reduced.set(true);
        d.push_event(egui::Event::WindowFocused(false));
        d.frame();
        assert_eq!(reads.get(), 1, "losing focus reads nothing");
        d.push_event(egui::Event::WindowFocused(true));
        d.frame();
        assert_eq!(reads.get(), 2, "read again on focus");
        assert!(motion::prefs(d.ctx()).reduced);
        assert_eq!(tokens::current(d.ctx()).accent, Color32::from_rgb(200, 40, 120));
    }
}
