//! Where the window was: saved in the settings under `"window"` and restored on the next launch
//! (see [`App::remember_window`](crate::App::remember_window)).
//!
//! Everything is in points (egui's logical units). What is read back is hostile: [`clamp`] keeps
//! the size sane and the window reachable on a monitor before it reaches the window builder.

use egui::{Pos2, Rect, Vec2};
use serde::{Deserialize, Serialize};

/// The settings key.
pub const KEY: &str = "window";
/// At least this much of the window stays on a monitor, so it can be grabbed and moved back.
pub const REACHABLE: f32 = 64.0;
/// The smallest window restored.
pub const MIN_SIZE: Vec2 = Vec2::new(200.0, 120.0);
/// No window is restored larger than this (points), whatever the file says.
pub const MAX_SIZE: f32 = 16_384.0;
/// Positions farther than this from the origin are noise.
const MAX_COORD: f32 = 100_000.0;

/// The window's placement, as saved.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowState {
    /// The outer top-left corner (with the title bar). `None` where the OS does not say (Wayland).
    pub pos: Option<[f32; 2]>,
    /// The content size.
    pub inner_size: Option<[f32; 2]>,
    pub maximized: bool,
    pub fullscreen: bool,
    /// The size of the monitor the window was on, a fallback when no monitor shows it any more.
    pub monitor: Option<[f32; 2]>,
}

/// A monitor's area in points, in the desktop's coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Monitor {
    pub rect: Rect,
}

impl Monitor {
    /// A monitor whose size is `physical` pixels at `scale` pixels per point, its top left at
    /// `origin` (points).
    pub fn from_pixels(origin: Pos2, physical: Vec2, scale: f32) -> Self {
        let scale = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
        Self { rect: Rect::from_min_size(origin, physical / scale) }
    }
}

fn finite(v: [f32; 2]) -> Option<Vec2> {
    let v = Vec2::new(v[0], v[1]);
    (v.x.is_finite() && v.y.is_finite()).then_some(v)
}

/// `state` made safe to open: sizes bounded, nonsense dropped, and, when `monitors` are known,
/// at least [`REACHABLE`] points of the window (its top edge included) on one of them. A window
/// no monitor shows moves onto the one it overlaps most, else the first (the primary), shrunk
/// to fit it.
pub fn clamp(state: WindowState, monitors: &[Monitor]) -> WindowState {
    let mut s = state;
    s.inner_size = s.inner_size.and_then(finite).map(|v| v.clamp(MIN_SIZE, Vec2::splat(MAX_SIZE)).into());
    s.pos = s.pos.and_then(finite).filter(|p| p.x.abs() <= MAX_COORD && p.y.abs() <= MAX_COORD).map(Into::into);
    s.monitor = s.monitor.and_then(finite).filter(|m| m.x >= 1.0 && m.y >= 1.0 && m.x <= MAX_SIZE && m.y <= MAX_SIZE).map(Into::into);
    let monitors: Vec<Rect> = monitors.iter().map(|m| m.rect).filter(|r| r.is_finite() && r.width() >= 1.0 && r.height() >= 1.0).collect();
    let (Some(pos), Some(first)) = (s.pos, monitors.first()) else { return s };
    let size = s.inner_size.map_or(MIN_SIZE, Vec2::from);
    let window = Rect::from_min_size(Pos2::from(pos), size);
    let overlap = |m: &Rect| {
        let i = m.intersect(window);
        if i.is_positive() { i.area() } else { 0.0 }
    };
    let best = monitors.iter().copied().max_by(|a, b| overlap(a).total_cmp(&overlap(b)));
    let home = best.filter(|m| overlap(m) > 0.0).unwrap_or(*first);
    // shrink to the monitor (never below the minimum, never above the monitor where it fits)
    let fit = size.min(home.size()).max(MIN_SIZE.min(home.size()));
    let reachable = {
        let i = home.intersect(Rect::from_min_size(window.min, fit));
        i.width() >= REACHABLE.min(fit.x) && i.height() >= REACHABLE.min(fit.y) && window.min.y >= home.min.y
    };
    if fit != size {
        s.inner_size = Some(fit.into());
    }
    if !reachable {
        let x = window.min.x.clamp(home.min.x, (home.max.x - fit.x).max(home.min.x));
        let y = window.min.y.clamp(home.min.y, (home.max.y - fit.y).max(home.min.y));
        s.pos = Some([x, y]);
    }
    s
}

/// The placement `info` shows, keeping `last`'s position and size while the window is maximized,
/// full screen or minimized (so leaving those restores the window where it was).
pub fn observe(info: &egui::ViewportInfo, last: &WindowState) -> WindowState {
    let maximized = info.maximized.unwrap_or(false);
    let fullscreen = info.fullscreen.unwrap_or(false);
    let normal = !maximized && !fullscreen && !info.minimized.unwrap_or(false);
    let mut s = WindowState { maximized, fullscreen, ..*last };
    if normal {
        if let Some(r) = info.outer_rect {
            s.pos = Some(r.min.into());
        }
        if let Some(r) = info.inner_rect {
            s.inner_size = Some(r.size().into());
        }
    }
    if let Some(m) = info.monitor_size {
        s.monitor = Some(m.into());
    }
    s
}

/// Follows the window and says when its placement needs saving (and, once, moving).
#[derive(Debug, Default)]
pub struct Tracker {
    last: Option<WindowState>,
    checked: bool,
}

/// What [`Tracker::update`] found.
#[derive(Debug, Default, PartialEq)]
pub struct Update {
    /// The placement changed: save this.
    pub save: Option<WindowState>,
    /// The window shows on no monitor: move it here.
    pub move_to: Option<[f32; 2]>,
}

impl Tracker {
    /// Compare the window (`info`) with what was saved (`saved` is read once, on the first call).
    /// Nothing is reported while the placement stays the same. Once the window has a position,
    /// a window winit finds on no monitor (entirely off screen) is moved back onto the monitor
    /// it was last on, assumed at the origin (the primary).
    pub fn update(&mut self, info: &egui::ViewportInfo, saved: impl FnOnce() -> WindowState) -> Update {
        let last = self.last.unwrap_or_else(saved);
        let state = observe(info, &last);
        let mut out = Update::default();
        if !self.checked && info.outer_rect.is_some() {
            self.checked = true;
            if info.monitor_size.is_none() && !state.maximized && !state.fullscreen {
                let size = state.monitor.map_or(Vec2::new(1280.0, 800.0), Vec2::from);
                let home = Monitor { rect: Rect::from_min_size(Pos2::ZERO, size) };
                out.move_to = clamp(state, &[home]).pos.filter(|p| Some(*p) != state.pos);
            }
        }
        if self.last != Some(state) {
            self.last = Some(state);
            out.save = Some(state);
        }
        out
    }
}

/// The builder with `state` applied (after [`clamp`]).
pub fn apply(mut b: egui::ViewportBuilder, state: &WindowState) -> egui::ViewportBuilder {
    if let Some(size) = state.inner_size {
        b = b.with_inner_size(size);
    }
    if let Some(pos) = state.pos {
        b = b.with_position(pos);
    }
    if state.maximized {
        b = b.with_maximized(true);
    }
    if state.fullscreen {
        b = b.with_fullscreen(true);
    }
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(x: f32, y: f32, w: f32, h: f32) -> WindowState {
        WindowState { pos: Some([x, y]), inner_size: Some([w, h]), ..Default::default() }
    }

    fn screen(x: f32, y: f32, w: f32, h: f32) -> Monitor {
        Monitor { rect: Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, h)) }
    }

    #[test]
    fn a_visible_window_is_left_alone() {
        let s = at(100.0, 80.0, 800.0, 600.0);
        assert_eq!(clamp(s, &[screen(0.0, 0.0, 1440.0, 900.0)]), s);
    }

    #[test]
    fn an_off_screen_window_comes_back() {
        let main = screen(0.0, 0.0, 1440.0, 900.0);
        let s = clamp(at(5000.0, 3000.0, 800.0, 600.0), &[main]);
        assert_eq!(s.pos, Some([640.0, 300.0]), "pulled into the bottom-right corner");
        assert_eq!(s.inner_size, Some([800.0, 600.0]));
        // only a sliver showing: not enough to grab
        let s = clamp(at(1440.0 - 10.0, 100.0, 800.0, 600.0), &[main]);
        assert_eq!(s.pos, Some([640.0, 100.0]));
        // 64 points showing is enough
        let s = at(1440.0 - REACHABLE, 100.0, 800.0, 600.0);
        assert_eq!(clamp(s, &[main]), s);
    }

    #[test]
    fn a_title_bar_above_the_screen_comes_down() {
        let s = clamp(at(100.0, -40.0, 800.0, 600.0), &[screen(0.0, 0.0, 1440.0, 900.0)]);
        assert_eq!(s.pos, Some([100.0, 0.0]));
    }

    #[test]
    fn negative_coordinates_on_a_monitor_left_of_the_primary_are_kept() {
        let monitors = [screen(0.0, 0.0, 1440.0, 900.0), screen(-1920.0, -200.0, 1920.0, 1080.0)];
        let s = at(-1500.0, -100.0, 800.0, 600.0);
        assert_eq!(clamp(s, &monitors), s);
        // that monitor unplugged: back on the primary
        let s = clamp(s, &monitors[..1]);
        assert_eq!(s.pos, Some([0.0, 0.0]));
    }

    #[test]
    fn mixed_dpi_monitors_clamp_in_points() {
        // a 1x 1920x1080 primary and a 2x 3840x2160 monitor right of it (1920x1080 points)
        let monitors = [
            Monitor::from_pixels(Pos2::ZERO, Vec2::new(1920.0, 1080.0), 1.0),
            Monitor::from_pixels(Pos2::new(1920.0, 0.0), Vec2::new(3840.0, 2160.0), 2.0),
        ];
        // saved at 3000 points wide on a monitor that was larger: shrunk to the 2x monitor's points
        let s = clamp(at(2000.0, 50.0, 3000.0, 1500.0), &monitors);
        assert_eq!(s.inner_size, Some([1920.0, 1080.0]));
        assert_eq!(s.pos, Some([2000.0, 50.0]), "64 points still show: left where it was");
        // a hostile scale is read as 1
        assert_eq!(Monitor::from_pixels(Pos2::ZERO, Vec2::splat(100.0), f32::NAN).rect.size(), Vec2::splat(100.0));
        assert_eq!(Monitor::from_pixels(Pos2::ZERO, Vec2::splat(100.0), 0.0).rect.size(), Vec2::splat(100.0));
    }

    #[test]
    fn absurd_values_are_bounded_or_dropped() {
        let s = clamp(
            WindowState { pos: Some([f32::NAN, 0.0]), inner_size: Some([1e9, -5.0]), monitor: Some([f32::INFINITY, 1.0]), ..Default::default() },
            &[],
        );
        assert_eq!(s.pos, None, "a NaN position is dropped");
        assert_eq!(s.inner_size, Some([MAX_SIZE, MIN_SIZE.y]));
        assert_eq!(s.monitor, None);
        assert_eq!(clamp(at(1e7, 0.0, 800.0, 600.0), &[]).pos, None, "too far to be real");
        assert_eq!(clamp(WindowState { inner_size: Some([f32::NAN, 10.0]), ..Default::default() }, &[]).inner_size, None);
        // with a monitor: a huge window fits the monitor
        let s = clamp(at(0.0, 0.0, 99_999.0, 99_999.0), &[screen(0.0, 0.0, 1440.0, 900.0)]);
        assert_eq!(s.inner_size, Some([1440.0, 900.0]));
        // a tiny monitor never makes a window larger than it
        let s = clamp(at(0.0, 0.0, 800.0, 600.0), &[screen(0.0, 0.0, 100.0, 50.0)]);
        assert_eq!(s.inner_size, Some([100.0, 50.0]));
        // degenerate monitors are ignored
        let s = at(5000.0, 5000.0, 800.0, 600.0);
        assert_eq!(clamp(s, &[screen(0.0, 0.0, 0.0, 0.0), Monitor { rect: Rect::NAN }]), s);
    }

    #[test]
    fn the_state_round_trips_and_old_files_still_load() {
        let s = WindowState { pos: Some([1.0, 2.0]), inner_size: Some([3.0, 4.0]), maximized: true, fullscreen: false, monitor: Some([5.0, 6.0]) };
        let v = serde_json::to_value(s).unwrap();
        assert_eq!(serde_json::from_value::<WindowState>(v).unwrap(), s);
        assert_eq!(serde_json::from_str::<WindowState>("{\"maximized\":true}").unwrap(), WindowState { maximized: true, ..Default::default() });
    }

    #[test]
    fn maximized_windows_keep_their_normal_placement() {
        let last = at(10.0, 20.0, 800.0, 600.0);
        let mut info = egui::ViewportInfo {
            outer_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1440.0, 900.0))),
            inner_rect: Some(Rect::from_min_size(Pos2::new(0.0, 28.0), Vec2::new(1440.0, 872.0))),
            maximized: Some(true),
            monitor_size: Some(Vec2::new(1440.0, 900.0)),
            ..Default::default()
        };
        let s = observe(&info, &last);
        assert!(s.maximized);
        assert_eq!((s.pos, s.inner_size), (last.pos, last.inner_size));
        assert_eq!(s.monitor, Some([1440.0, 900.0]));
        info.maximized = Some(false);
        let s = observe(&info, &last);
        assert_eq!((s.pos, s.inner_size), (Some([0.0, 0.0]), Some([1440.0, 872.0])));
    }

    fn window(x: f32, y: f32, monitor: Option<Vec2>) -> egui::ViewportInfo {
        egui::ViewportInfo {
            outer_rect: Some(Rect::from_min_size(Pos2::new(x, y), Vec2::new(800.0, 628.0))),
            inner_rect: Some(Rect::from_min_size(Pos2::new(x, y + 28.0), Vec2::new(800.0, 600.0))),
            monitor_size: monitor,
            ..Default::default()
        }
    }

    #[test]
    fn the_tracker_saves_only_changes() {
        let mut t = Tracker::default();
        let screen = Some(Vec2::new(1440.0, 900.0));
        let mut reads = 0;
        let u = t.update(&window(10.0, 20.0, screen), || {
            reads += 1;
            WindowState::default()
        });
        assert_eq!(u.save.map(|s| (s.pos, s.inner_size)), Some((Some([10.0, 20.0]), Some([800.0, 600.0]))));
        assert_eq!(u.move_to, None, "on a monitor: stays");
        assert_eq!(t.update(&window(10.0, 20.0, screen), WindowState::default), Update::default(), "unchanged: nothing to write");
        assert_eq!(t.update(&window(30.0, 20.0, screen), WindowState::default).save.and_then(|s| s.pos), Some([30.0, 20.0]));
        assert_eq!(reads, 1, "the settings are read once");
    }

    #[test]
    fn the_tracker_brings_back_a_window_on_no_monitor() {
        let mut t = Tracker::default();
        // waiting for a position: nothing checked yet
        let u = t.update(&egui::ViewportInfo::default(), || WindowState { monitor: Some([1440.0, 900.0]), ..Default::default() });
        assert_eq!(u.move_to, None);
        let u = t.update(&window(4000.0, 3000.0, None), WindowState::default);
        assert_eq!(u.move_to, Some([640.0, 300.0]), "onto the monitor it was last on");
        // checked once only
        assert_eq!(t.update(&window(4000.0, 2000.0, None), WindowState::default).move_to, None);
    }
}
