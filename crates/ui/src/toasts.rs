//! Toast notifications: a bounded queue the app pushes to, painted in a corner.
//!
//! Timed toasts disappear after their duration (the timer pauses while the pointer is over them or
//! keyboard focus is inside them); sticky ones stay until dismissed. Clicking a toast's close
//! button, or pressing Escape, dismisses it. A toast can carry a title, a progress bar and up to
//! [`MAX_ACTIONS`] action buttons; a toast pushed with the [`key`](Toast::key) of a queued one
//! replaces it in place. Toasts beyond [`Toasts::max_visible`] are summed up as "+N more".
//!
//! Each toast is an AccessKit live region (polite, or assertive for errors), so screen readers
//! announce it. An empty queue costs nothing per frame; a visible timed toast schedules exactly
//! one repaint at its expiry, and a stack of sticky toasts requests no frames at all.
//!
//! A new toast fades and slides 8 pt in; a dismissed one slides back out over the exit spring
//! and is painted until that settles, while the toasts behind it glide into the gap. Toasts
//! already queued when the stack is first shown appear without animation. Hit testing and
//! automation rects always use the resting layout; under reduced motion only the fade remains.

use std::collections::VecDeque;
use std::time::Duration;

use egui::accesskit::{Live, Role as AkRole};
use egui::{Align2, Id, Key, Modifiers, Order, Rect, Sense, Stroke, StrokeKind, Ui, Vec2, pos2, text::LayoutJob, vec2};

use crate::automation::{self, Details, Role};
use crate::tokens::{self, Tokens};
use ashlar_motion::tokens as tokens_motion;

/// How important a toast is (sets its accent colour).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToastLevel {
    #[default]
    Info,
    Success,
    Warning,
    Error,
}

/// A button on a toast: clicking it reports [`ToastActivation`] with this `command` (see
/// [`Toasts::take_actions`]) and dismisses the toast.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToastAction {
    pub label: String,
    /// What the app should do, usually a command id (`"export.reveal"`).
    pub command: String,
}

/// One notification.
#[derive(Clone, Debug, PartialEq)]
pub struct Toast {
    pub level: ToastLevel,
    /// The message (shown under the title when there is one).
    pub text: String,
    /// `None`: stays until dismissed.
    pub duration: Option<Duration>,
    /// Optional heading above the text.
    pub title: Option<String>,
    /// Buttons, at most [`MAX_ACTIONS`] (more are dropped).
    pub actions: Vec<ToastAction>,
    /// Pushing a toast with the key of a queued one replaces that toast in place.
    pub key: Option<String>,
    /// Draws a progress bar filled to this fraction (`0.0..=1.0`).
    pub progress: Option<f32>,
}

impl Toast {
    pub fn new(level: ToastLevel, text: impl Into<String>) -> Self {
        let duration = Some(if level == ToastLevel::Error { Duration::from_secs(8) } else { Duration::from_secs(4) });
        Toast { level, text: text.into(), duration, title: None, actions: Vec::new(), key: None, progress: None }
    }
    pub fn info(text: impl Into<String>) -> Self {
        Self::new(ToastLevel::Info, text)
    }
    pub fn success(text: impl Into<String>) -> Self {
        Self::new(ToastLevel::Success, text)
    }
    pub fn warning(text: impl Into<String>) -> Self {
        Self::new(ToastLevel::Warning, text)
    }
    pub fn error(text: impl Into<String>) -> Self {
        Self::new(ToastLevel::Error, text)
    }
    pub fn sticky(mut self) -> Self {
        self.duration = None;
        self
    }
    pub fn duration(mut self, d: Duration) -> Self {
        self.duration = Some(d);
        self
    }
    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }
    /// Add a button; clicking it reports `command` from [`Toasts::take_actions`].
    pub fn action(mut self, label: impl Into<String>, command: impl Into<String>) -> Self {
        self.actions.push(ToastAction { label: label.into(), command: command.into() });
        self
    }
    /// Replace a queued toast with the same key instead of adding another one.
    pub fn key(mut self, key: impl Into<String>) -> Self {
        self.key = Some(key.into());
        self
    }
    /// Show a progress bar filled to `fraction` (clamped to `0.0..=1.0`; non-finite counts as 0).
    pub fn progress(mut self, fraction: f32) -> Self {
        self.progress = Some(fraction);
        self
    }
}

/// An action button the user clicked (from [`Toasts::take_actions`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToastActivation {
    /// The toast's id (as returned by [`Toasts::push`]).
    pub toast: u64,
    /// The toast's key, when it had one.
    pub key: Option<String>,
    /// Which of the toast's actions (0-based).
    pub index: usize,
    pub command: String,
}

/// Words the toast stack shows (localized by the app).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToastLabels {
    /// The close button.
    pub dismiss: String,
    /// The overflow line; `{n}` is replaced by the number of hidden toasts.
    pub more: String,
}

impl Default for ToastLabels {
    fn default() -> Self {
        ToastLabels { dismiss: "Dismiss".into(), more: "+{n} more".into() }
    }
}

struct Entry {
    id: u64,
    toast: Toast,
    /// Seconds of display time left (timed toasts only).
    remaining: Option<f64>,
    /// Bumped when the toast is replaced in place (keys its cached text layout).
    rev: u64,
    /// Where it was drawn last frame (signed offset from the corner); `None` if it was not drawn.
    drawn: Option<f32>,
}

/// The most dismissed toasts kept painting their exit at once.
const MAX_LEAVING: usize = 16;
/// How far (points) a toast slides as it enters and leaves.
const SLIDE: f32 = 8.0;

/// Which corner toasts stack from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Corner {
    TopRight,
    #[default]
    BottomRight,
    BottomLeft,
    TopLeft,
}

/// The queue. Keep one per window (the app owns it).
pub struct Toasts {
    entries: VecDeque<Entry>,
    next_id: u64,
    capacity: usize,
    max_visible: usize,
    corner: Corner,
    width: f32,
    inset: Vec2,
    last_time: Option<f64>,
    labels: ToastLabels,
    activated: Vec<ToastActivation>,
    /// At the end of the last frame, nothing else wanted Escape (no popup, no focus outside toasts).
    escape_ready: bool,
    /// The toast that held keyboard focus at the end of the last frame.
    focused: Option<u64>,
    /// Dismissed toasts still painting their exit.
    leaving: Vec<Entry>,
    /// `show` has run: toasts pushed from now on animate in.
    seen: bool,
}

impl Default for Toasts {
    fn default() -> Self {
        Self::new()
    }
}

/// Longest toast text kept (longer text is cut, so a runaway message cannot fill memory).
pub const MAX_TEXT: usize = 2000;
/// Most action buttons on one toast.
pub const MAX_ACTIONS: usize = 3;
/// Longest title, action label, command or key kept.
pub const MAX_LABEL: usize = 200;
/// Most unread activations kept (the oldest are dropped first).
const MAX_ACTIVATIONS: usize = 32;

/// Cut `s` to at most `max` bytes on a char boundary, marking the cut with an ellipsis.
fn cap(s: &mut String, max: usize) {
    if s.len() > max {
        let mut cut = max;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
        s.push('…');
    }
}

fn sanitize(toast: &mut Toast) {
    cap(&mut toast.text, MAX_TEXT);
    if let Some(title) = toast.title.as_mut() {
        cap(title, MAX_LABEL);
    }
    if let Some(key) = toast.key.as_mut() {
        cap(key, MAX_LABEL);
    }
    toast.actions.truncate(MAX_ACTIONS);
    for a in &mut toast.actions {
        cap(&mut a.label, MAX_LABEL);
        cap(&mut a.command, MAX_LABEL);
    }
    toast.progress = toast.progress.map(|p| if p.is_finite() { p.clamp(0.0, 1.0) } else { 0.0 });
}

impl Toasts {
    pub fn new() -> Self {
        Toasts {
            entries: VecDeque::new(),
            next_id: 1,
            capacity: 32,
            max_visible: 4,
            corner: Corner::default(),
            width: 320.0,
            inset: Vec2::ZERO,
            last_time: None,
            labels: ToastLabels::default(),
            activated: Vec::new(),
            escape_ready: true,
            focused: None,
            leaving: Vec::new(),
            seen: false,
        }
    }
    /// Keep at most `capacity` toasts (the oldest are dropped first).
    pub fn capacity(mut self, capacity: usize) -> Self {
        self.capacity = capacity.clamp(1, 1024);
        self
    }
    /// Paint at most `n` toasts; the rest are summed up in a "+N more" line.
    pub fn max_visible(mut self, n: usize) -> Self {
        self.max_visible = n.clamp(1, 16);
        self
    }
    pub fn corner(mut self, corner: Corner) -> Self {
        self.corner = corner;
        self
    }
    /// Keep the stack this far (in points) further from its corner's edges than the usual margin,
    /// clear of a status bar or a toolbar. Negative or non-finite parts count as zero.
    pub fn inset(mut self, inset: Vec2) -> Self {
        let clean = |v: f32| if v.is_finite() { v.clamp(0.0, 10_000.0) } else { 0.0 };
        self.inset = Vec2::new(clean(inset.x), clean(inset.y));
        self
    }
    pub fn width(mut self, width: f32) -> Self {
        if width.is_finite() {
            self.width = width.clamp(120.0, 1200.0);
        }
        self
    }
    /// Words the stack shows (English by default).
    pub fn labels(mut self, labels: ToastLabels) -> Self {
        self.labels = labels;
        self
    }

    /// Queue a toast; returns its id (for [`Toasts::dismiss`]). A toast whose key matches a queued
    /// toast replaces that one in place (same id, same position, timer restarted).
    pub fn push(&mut self, mut toast: Toast) -> u64 {
        sanitize(&mut toast);
        let remaining = toast.duration.map(|d| d.as_secs_f64().min(3600.0));
        if let Some(key) = toast.key.as_deref()
            && let Some(e) = self.entries.iter_mut().find(|e| e.toast.key.as_deref() == Some(key))
        {
            e.toast = toast;
            e.remaining = remaining;
            e.rev = e.rev.wrapping_add(1);
            return e.id;
        }
        while self.entries.len() >= self.capacity {
            if let Some(e) = self.entries.pop_front() {
                self.retire(e);
            }
        }
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        self.entries.push_back(Entry { id, toast, remaining, rev: 0, drawn: None });
        id
    }
    pub fn info(&mut self, text: impl Into<String>) -> u64 {
        self.push(Toast::new(ToastLevel::Info, text))
    }
    pub fn success(&mut self, text: impl Into<String>) -> u64 {
        self.push(Toast::new(ToastLevel::Success, text))
    }
    pub fn warning(&mut self, text: impl Into<String>) -> u64 {
        self.push(Toast::new(ToastLevel::Warning, text))
    }
    pub fn error(&mut self, text: impl Into<String>) -> u64 {
        self.push(Toast::new(ToastLevel::Error, text))
    }
    /// Show or update a sticky progress toast for `key`: call it again with the same key to move
    /// the bar (`None` hides it), then push a finished toast with that key or
    /// [`dismiss_key`](Toasts::dismiss_key) it.
    pub fn progress(&mut self, key: impl Into<String>, text: impl Into<String>, fraction: Option<f32>) -> u64 {
        let mut toast = Toast::new(ToastLevel::Info, text).key(key).sticky();
        toast.progress = fraction;
        self.push(toast)
    }
    pub fn dismiss(&mut self, id: u64) -> bool {
        self.remove_where(|e| e.id == id)
    }
    /// Dismiss the toast with this key.
    pub fn dismiss_key(&mut self, key: &str) -> bool {
        self.remove_where(|e| e.toast.key.as_deref() == Some(key))
    }
    pub fn clear(&mut self) {
        self.remove_where(|_| true);
    }

    /// Take the matching toasts out of the queue (they leave with an exit animation); whether any
    /// matched.
    fn remove_where(&mut self, mut gone: impl FnMut(&Entry) -> bool) -> bool {
        if !self.entries.iter().any(&mut gone) {
            return false;
        }
        for e in std::mem::take(&mut self.entries) {
            if gone(&e) {
                self.retire(e);
            } else {
                self.entries.push_back(e);
            }
        }
        true
    }

    /// A toast leaving the queue: one that was on screen keeps painting its exit.
    fn retire(&mut self, mut e: Entry) {
        if e.drawn.is_none() {
            return;
        }
        if self.leaving.len() >= MAX_LEAVING {
            self.leaving.remove(0);
        }
        e.remaining = None;
        self.leaving.push(e);
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    /// Texts of queued toasts, oldest first.
    pub fn texts(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|e| e.toast.text.as_str())
    }
    /// The queued toast with this id.
    pub fn get(&self, id: u64) -> Option<&Toast> {
        self.entries.iter().find(|e| e.id == id).map(|e| &e.toast)
    }
    /// Action buttons clicked since the last call, oldest first.
    pub fn take_actions(&mut self) -> Vec<ToastActivation> {
        std::mem::take(&mut self.activated)
    }

    /// [`show`](Toasts::show), then [`take_actions`](Toasts::take_actions).
    pub fn show_actions(&mut self, ctx: &egui::Context) -> Vec<ToastActivation> {
        self.show(ctx);
        self.take_actions()
    }

    /// Paint the visible toasts and age them. Call once per frame, after the main UI. Clicked
    /// actions wait in [`take_actions`](Toasts::take_actions).
    pub fn show(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        let dt = self.last_time.map_or(0.0, |last| (now - last).clamp(0.0, 1.0));
        self.last_time = Some(now);
        let seen = std::mem::replace(&mut self.seen, true);
        if self.entries.is_empty() && self.leaving.is_empty() {
            self.last_time = None;
            self.focused = None;
            self.escape_ready = true;
            return;
        }
        let t = tokens::current(ctx);
        let screen = ctx.content_rect();
        let margin = t.space.lg;
        let (anchor, down) = match self.corner {
            Corner::TopRight => (Align2::RIGHT_TOP, true),
            Corner::BottomRight => (Align2::RIGHT_BOTTOM, false),
            Corner::BottomLeft => (Align2::LEFT_BOTTOM, false),
            Corner::TopLeft => (Align2::LEFT_TOP, true),
        };
        let width = self.width.min((screen.width() - 2.0 * margin).max(80.0));
        let visible = self.entries.len().min(self.max_visible);
        let x = if matches!(anchor, Align2::RIGHT_TOP | Align2::RIGHT_BOTTOM) { -margin - self.inset.x } else { margin + self.inset.x };
        let y_margin = if down { margin + self.inset.y } else { -margin - self.inset.y };

        // Escape dismisses the focused toast, else the newest visible one, unless something else
        // (a popup, a focused field) had a claim on it when the key was pressed.
        let mut dismiss = None;
        if visible > 0 && self.escape_ready && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            let focused = self.focused.filter(|id| self.entries.iter().take(visible).any(|e| e.id == *id));
            dismiss = focused.or_else(|| self.entries.get(visible.saturating_sub(1)).map(|e| e.id));
        }

        // toasts come in from, and leave toward, the edge they stack from
        let reduced = crate::motion::prefs(ctx).reduced;
        let slide = |p: f32| if reduced { 0.0 } else { (1.0 - p) * SLIDE * if down { -1.0 } else { 1.0 } };

        // dismissed toasts: painted where they were, sliding and fading out, never interactive
        self.leaving.retain_mut(|entry| {
            let area_id = Id::new(("ashlar_ui::toast", entry.id));
            let Some(p) = crate::motion::presence(ctx, area_id.with("presence"), false, tokens_motion::SNAP) else { return false };
            let y = entry.drawn.unwrap_or(0.0);
            let area = egui::Area::new(area_id).order(Order::Foreground).anchor(anchor, vec2(x, y + y_margin)).interactable(false).fade_in(false);
            area.show(ctx, |ui| {
                ui.set_width(width);
                paint_toast(ui, &t, &self.labels, entry, Look { live: false, shift: vec2(0.0, slide(p)), alpha: p });
            });
            true
        });

        let mut offset = 0.0f32;
        let mut next_expiry: Option<f64> = None;
        let mut focused = None;
        for (n, entry) in self.entries.iter_mut().enumerate() {
            if n >= visible {
                entry.drawn = None;
                continue;
            }
            let area_id = Id::new(("ashlar_ui::toast", entry.id));
            let y = if down { offset } else { -offset };
            // the toasts behind a dismissed one glide into the gap; the area (hit testing and
            // automation) sits at the resting place, only the paint moves
            let drawn = crate::motion::f32(ctx, area_id.with("y"), y, tokens_motion::REFLOW.enter);
            let presence_id = area_id.with("presence");
            if entry.drawn.is_none() && seen {
                crate::motion::set(ctx, presence_id, 0.0_f32);
            }
            let p = crate::motion::presence(ctx, presence_id, true, tokens_motion::SNAP).unwrap_or(1.0);
            entry.drawn = Some(drawn);
            let look = Look { live: true, shift: vec2(0.0, drawn - y + slide(p)), alpha: p };
            let area = egui::Area::new(area_id).order(Order::Foreground).anchor(anchor, vec2(x, y + y_margin)).interactable(true).fade_in(false);
            let mut hovered = false;
            let inner = area.show(ctx, |ui| {
                ui.set_width(width);
                let painted = paint_toast(ui, &t, &self.labels, entry, look);
                hovered = ui.rect_contains_pointer(ui.min_rect());
                painted
            });
            let painted = inner.inner;
            offset += painted.height + t.space.sm;
            if painted.focused {
                focused = Some(entry.id);
            }
            if painted.closed {
                dismiss.get_or_insert(entry.id);
            }
            if let Some(index) = painted.action {
                if let Some(action) = entry.toast.actions.get(index) {
                    if self.activated.len() >= MAX_ACTIVATIONS {
                        self.activated.remove(0);
                    }
                    self.activated.push(ToastActivation { toast: entry.id, key: entry.toast.key.clone(), index, command: action.command.clone() });
                }
                dismiss.get_or_insert(entry.id);
            }
            if let Some(rem) = entry.remaining.as_mut() {
                if !hovered && !painted.focused {
                    *rem -= dt;
                }
                if *rem <= 0.0 {
                    dismiss.get_or_insert(entry.id);
                } else {
                    next_expiry = Some(next_expiry.map_or(*rem, |n: f64| n.min(*rem)));
                }
            }
        }
        let hidden = self.entries.len().saturating_sub(visible);
        if hidden > 0 {
            let y = if down { offset } else { -offset };
            let area =
                egui::Area::new(Id::new("ashlar_ui::toast_more")).order(Order::Foreground).anchor(anchor, vec2(x, y + y_margin)).interactable(true);
            area.show(ctx, |ui| {
                ui.set_width(width);
                paint_more(ui, &t, &self.labels, hidden, matches!(anchor, Align2::LEFT_TOP | Align2::LEFT_BOTTOM));
            });
        }

        self.remove_where(|e| Some(e.id) == dismiss || e.remaining.is_some_and(|r| r <= 0.0));
        self.focused = focused.filter(|id| Some(*id) != dismiss);
        let elsewhere = ctx.memory(|m| m.focused().is_some()) && focused.is_none();
        self.escape_ready = !elsewhere && !egui::Popup::is_any_open(ctx);
        if dismiss.is_some() {
            crate::wake::wake(ctx, crate::wake::Reason::INPUT);
        } else if let Some(secs) = next_expiry {
            crate::wake::wake_after(ctx, crate::wake::Reason::TIMER, Duration::from_secs_f64(secs.max(0.016)));
        }
    }
}

/// What one painted toast reported.
struct Painted {
    height: f32,
    closed: bool,
    /// Index of the clicked action.
    action: Option<usize>,
    /// Keyboard focus is on one of its controls.
    focused: bool,
}

fn focus_ring(painter: &egui::Painter, t: &Tokens, rect: Rect) {
    painter.rect_stroke(rect, t.radius.sm, Stroke::new(2.0, t.focus), StrokeKind::Inside);
}

/// The close button's cross.
fn paint_close(painter: &egui::Painter, rect: Rect, col: egui::Color32) {
    let c = rect.center();
    let k = rect.width() * 0.18;
    painter.line_segment([c - vec2(k, k), c + vec2(k, k)], Stroke::new(1.5, col));
    painter.line_segment([c + vec2(-k, k), c + vec2(k, -k)], Stroke::new(1.5, col));
}

/// How a toast is drawn this frame.
#[derive(Clone, Copy)]
struct Look {
    /// In the queue: interactive and reported to automation and screen readers.
    live: bool,
    /// Paint offset from the resting rect (which hit testing keeps using).
    shift: Vec2,
    /// Opacity 0..=1.
    alpha: f32,
}

fn paint_toast(ui: &mut Ui, t: &Tokens, labels: &ToastLabels, entry: &Entry, look: Look) -> Painted {
    let toast = &entry.toast;
    let accent = match toast.level {
        ToastLevel::Info => t.status.info,
        ToastLevel::Success => t.status.success,
        ToastLevel::Warning => t.status.warning,
        ToastLevel::Error => t.status.danger,
    };
    let base = Id::new(("ashlar_ui::toast", entry.id));
    let width = ui.available_width();
    let pad = t.space.md;
    let gap = t.space.xs;
    let close_w = t.control_height;
    let text_x = pad + 6.0;
    let text_w = (width - text_x - pad - close_w - gap).max(40.0);
    let rev = entry.rev;

    let body_font = Tokens::font(t.type_scale.body);
    let revision = crate::cache::hash_of(&(text_w.to_bits(), body_font.size.to_bits(), t.text.primary, rev));
    let body = crate::cache::galley(ui, ("ashlar_ui::toast", entry.id), revision, || {
        LayoutJob::simple(toast.text.clone(), body_font.clone(), t.text.primary, text_w)
    });
    let title = toast.title.as_ref().map(|title| {
        let font = Tokens::font(t.type_scale.heading);
        let revision = crate::cache::hash_of(&(text_w.to_bits(), font.size.to_bits(), t.text.primary, rev));
        crate::cache::galley(ui, ("ashlar_ui::toast.title", entry.id), revision, || LayoutJob::simple(title.clone(), font, t.text.primary, text_w))
    });
    let button_font = Tokens::font(t.type_scale.button);
    let actions: Vec<_> = toast
        .actions
        .iter()
        .enumerate()
        .map(|(n, a)| {
            let revision = crate::cache::hash_of(&(button_font.size.to_bits(), t.text.primary, rev));
            crate::cache::galley(ui, ("ashlar_ui::toast.action", entry.id, n), revision, || {
                LayoutJob::simple(a.label.clone(), button_font.clone(), t.text.primary, text_w)
            })
        })
        .collect();

    let title_h = title.as_ref().map_or(0.0, |g| g.size().y + gap);
    let text_h = (title_h + body.size().y).max(close_w);
    let progress_h = if toast.progress.is_some() { gap * 2.0 + 4.0 } else { 0.0 };
    let actions_h = if actions.is_empty() { 0.0 } else { t.space.sm + t.control_height };
    let height = pad + text_h + progress_h + actions_h + pad;
    let rect = if look.live {
        ui.allocate_exact_size(vec2(width, height), Sense::hover()).0
    } else {
        // a leaving toast takes its space but is no widget
        let r = Rect::from_min_size(ui.cursor().min, vec2(width, height));
        ui.advance_cursor_after_rect(r);
        r
    };
    // paint at the drawn place; `rect` (hit testing, automation) stays at rest
    let shift = look.shift;
    let mut painter = ui.painter().clone();
    painter.multiply_opacity(look.alpha.clamp(0.0, 1.0));
    let painter = &painter;
    let drawn = rect.translate(shift);
    painter.add(egui::Shape::Rect(egui::epaint::RectShape::filled(drawn, t.radius.lg, t.surface.raised)));
    painter.rect_stroke(drawn, t.radius.lg, Stroke::new(1.0, t.border), StrokeKind::Inside);
    let bar = Rect::from_min_size(drawn.min, vec2(4.0, drawn.height()));
    painter.rect_filled(bar, egui::CornerRadius { nw: t.radius.lg, sw: t.radius.lg, ne: 0, se: 0 }, accent);
    let mut y = drawn.top() + pad;
    if let Some(title) = title {
        let h = title.size().y;
        painter.galley(pos2(drawn.left() + text_x, y), title, t.text.primary);
        y += h + gap;
    }
    painter.galley(pos2(drawn.left() + text_x, y), body, t.text.primary);
    if let Some(fraction) = toast.progress {
        let track = Rect::from_min_size(pos2(drawn.left() + text_x, drawn.top() + pad + text_h + gap), vec2(text_w, 4.0));
        painter.rect_filled(track, 2.0, t.surface.sunken);
        painter.rect_filled(Rect::from_min_size(track.min, vec2(track.width() * fraction, track.height())), 2.0, accent);
    }
    let close_rect = Rect::from_min_size(pos2(rect.right() - pad / 2.0 - close_w, rect.top() + pad / 2.0), Vec2::splat(close_w));
    if !look.live {
        paint_close(painter, close_rect.translate(shift), t.text.secondary);
        let mut right = drawn.right() - pad;
        let top = drawn.bottom() - pad - t.control_height;
        for galley in actions {
            let w = galley.size().x + 2.0 * pad;
            let r = Rect::from_min_size(pos2(right - w, top), vec2(w, t.control_height));
            right -= w + gap;
            painter.rect_filled(r, t.radius.md, t.surface.panel);
            painter.rect_stroke(r, t.radius.md, Stroke::new(1.0, t.border), StrokeKind::Inside);
            painter.galley(r.center() - galley.size() / 2.0, galley, t.text.primary);
        }
        return Painted { height, closed: false, action: None, focused: false };
    }

    // the whole toast: a live region screen readers announce
    let level = level_name(toast.level);
    let body_resp = ui.interact(rect, base.with("body"), Sense::hover());
    let name = toast.title.as_deref().unwrap_or(&toast.text);
    body_resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, true, name));
    let assertive = toast.level == ToastLevel::Error;
    ui.ctx().accesskit_node_builder(body_resp.id, |node| {
        node.set_role(if assertive { AkRole::Alert } else { AkRole::Status });
        node.set_live(if assertive { Live::Assertive } else { Live::Polite });
        if toast.title.is_some() {
            node.set_description(toast.text.as_str());
        }
        if let Some(fraction) = toast.progress {
            node.set_numeric_value(f64::from(fraction) * 100.0);
        }
    });
    let id = entry.id;
    automation::register_fmt(ui, rect, format_args!("toast.{id}"), &toast.text, Role::Toast, true, Details { value: Some(level), selected: None });

    // close button: a full control-height target in the top right corner
    let close = ui.interact(close_rect, base.with("close"), Sense::click());
    let col = if close.hovered() || close.has_focus() { t.text.primary } else { t.text.secondary };
    if close.hovered() {
        painter.rect_filled(close_rect.translate(shift), t.radius.sm, t.surface.hover);
    }
    if close.has_focus() {
        focus_ring(painter, t, close_rect.translate(shift));
    }
    paint_close(painter, close_rect.translate(shift), col);
    automation::register_fmt(ui, close_rect, format_args!("toast.{id}.close"), &labels.dismiss, Role::Button, true, Details::default());
    close.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &labels.dismiss));
    let mut focused = close.has_focus();

    // actions, right-aligned under the text
    let mut action = None;
    let mut right = rect.right() - pad;
    let top = rect.bottom() - pad - t.control_height;
    for (n, galley) in actions.into_iter().enumerate() {
        let Some(a) = toast.actions.get(n) else { continue };
        let w = galley.size().x + 2.0 * pad;
        let r = Rect::from_min_size(pos2(right - w, top), vec2(w, t.control_height));
        right -= w + gap;
        let resp = ui.interact(r, base.with(("action", n)), Sense::click());
        let fill = if resp.is_pointer_button_down_on() {
            t.surface.pressed
        } else if resp.hovered() {
            t.surface.hover
        } else {
            t.surface.panel
        };
        let d = r.translate(shift);
        painter.rect_filled(d, t.radius.md, fill);
        painter.rect_stroke(d, t.radius.md, Stroke::new(1.0, t.border), StrokeKind::Inside);
        let gpos = d.center() - galley.size() / 2.0;
        painter.galley(gpos, galley, t.text.primary);
        if resp.has_focus() {
            focus_ring(painter, t, d);
            focused = true;
        }
        automation::register_fmt(ui, r, format_args!("toast.{id}.action.{n}"), &a.label, Role::Button, true, Details::default());
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &a.label));
        if resp.clicked() {
            action = Some(n);
        }
    }

    Painted { height, closed: close.clicked(), action, focused }
}

/// The "+N more" line under the visible toasts.
fn paint_more(ui: &mut Ui, t: &Tokens, labels: &ToastLabels, hidden: usize, left: bool) {
    let text = labels.more.replace("{n}", &hidden.to_string());
    let font = Tokens::font(t.type_scale.small);
    let revision = crate::cache::hash_of(&(text.as_str(), font.size.to_bits(), t.text.secondary));
    let galley =
        crate::cache::galley(ui, "ashlar_ui::toast_more", revision, || LayoutJob::simple(text.clone(), font, t.text.secondary, f32::INFINITY));
    let pad = t.space.sm;
    let size = galley.size() + vec2(2.0 * pad, pad);
    let width = ui.available_width();
    // hug the stack's outer edge
    let (row, _) = ui.allocate_exact_size(vec2(width, size.y), Sense::hover());
    let pill = Rect::from_min_size(pos2(if left { row.left() } else { row.right() - size.x }, row.top()), size);
    ui.painter().rect_filled(pill, t.radius.lg, t.surface.raised);
    ui.painter().rect_stroke(pill, t.radius.lg, Stroke::new(1.0, t.border), StrokeKind::Inside);
    ui.painter().galley(pill.min + vec2(pad, pad / 2.0), galley, t.text.secondary);
    let resp = ui.interact(pill, Id::new("ashlar_ui::toast_more"), Sense::hover());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, &text));
    automation::register_fmt(ui, pill, format_args!("toast.more"), &text, Role::Status, true, Details::default());
}

fn level_name(l: ToastLevel) -> &'static str {
    match l {
        ToastLevel::Info => "info",
        ToastLevel::Success => "success",
        ToastLevel::Warning => "warning",
        ToastLevel::Error => "error",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::Harness;
    use egui_kittest::kittest::{NodeT, Queryable};

    #[test]
    fn queue_is_bounded_and_text_is_capped() {
        let mut q = Toasts::new().capacity(3);
        for i in 0..10 {
            q.info(format!("n{i}"));
        }
        assert_eq!(q.texts().collect::<Vec<_>>(), ["n7", "n8", "n9"]);
        let id = q.push(Toast::new(ToastLevel::Error, "é".repeat(MAX_TEXT)));
        assert!(q.texts().last().unwrap().len() <= MAX_TEXT + 3);
        assert!(q.dismiss(id));
        assert!(!q.dismiss(id));
    }

    #[test]
    fn inset_keeps_the_stack_clear_of_the_edges() {
        let shown = |toasts: Toasts| {
            let mut h = Harness::builder().with_size(vec2(600.0, 400.0)).build_ui_state(|ui, q: &mut Toasts| q.show(ui.ctx()), toasts);
            automation::install(&h.ctx);
            h.state_mut().info("Saved");
            // an area is measured on its first frame and placed on the next
            h.step();
            h.step();
            automation::query(&h.ctx, "toast.")
                .into_iter()
                .find(|e| e.label == "Saved")
                .map(|e| (e.rect[0] + e.rect[2], e.rect[1] + e.rect[3]))
                .unwrap()
        };
        let plain = shown(Toasts::new());
        let inset = shown(Toasts::new().inset(vec2(10.0, 30.0)));
        assert!((plain.0 - inset.0 - 10.0).abs() < 0.5, "{plain:?} {inset:?}");
        assert!((plain.1 - inset.1 - 30.0).abs() < 0.5, "{plain:?} {inset:?}");
        let hostile = shown(Toasts::new().inset(vec2(f32::NAN, -50.0)));
        assert_eq!(hostile, plain);
    }

    #[test]
    fn timed_toasts_expire_sticky_ones_stay_and_close_dismisses() {
        let mut h = Harness::builder().with_size(vec2(600.0, 400.0)).with_step_dt(0.5).build_ui_state(
            |ui, q: &mut Toasts| {
                q.show(ui.ctx());
            },
            Toasts::new(),
        );
        automation::install(&h.ctx);
        h.state_mut().push(Toast::new(ToastLevel::Info, "Saved").duration(Duration::from_secs(1)));
        let sticky = h.state_mut().push(Toast::new(ToastLevel::Warning, "Disk almost full").sticky());
        h.step();
        let saved = automation::query(&h.ctx, "toast.").into_iter().find(|e| e.label == "Saved").unwrap();
        assert_eq!(saved.value, "info");
        assert!(h.ctx.content_rect().contains(saved.center()));
        for _ in 0..4 {
            h.step();
        }
        assert_eq!(h.state().texts().collect::<Vec<_>>(), ["Disk almost full"]);
        let close = automation::find(&h.ctx, &format!("toast.{sticky}.close")).unwrap();
        h.hover_at(close.center());
        h.step();
        h.drag_at(close.center());
        h.step();
        h.drop_at(close.center());
        h.step();
        assert!(h.state().is_empty());
        // an empty queue settles: no more repaints are requested
        h.run();
    }

    fn harness(toasts: Toasts) -> Harness<'static, Toasts> {
        let h = Harness::builder().with_size(vec2(600.0, 400.0)).with_step_dt(0.5).build_ui_state(|ui, q: &mut Toasts| q.show(ui.ctx()), toasts);
        automation::install(&h.ctx);
        h
    }

    fn click(h: &mut Harness<'static, Toasts>, id: &str) {
        let at = automation::find(&h.ctx, id).unwrap_or_else(|| panic!("no {id}")).center();
        h.hover_at(at);
        h.step();
        h.drag_at(at);
        h.step();
        h.drop_at(at);
        h.step();
    }

    #[test]
    fn same_key_updates_in_place() {
        let mut h = harness(Toasts::new());
        let first = h.state_mut().push(Toast::success("Saved").key("save"));
        for _ in 0..4 {
            assert_eq!(h.state_mut().push(Toast::success("Saved").key("save")), first);
        }
        assert_eq!(h.state().len(), 1);

        // a progress toast moves its bar in place, then becomes the finished toast
        let job = h.state_mut().progress("job.42", "Rendering", Some(0.3));
        h.step();
        h.step();
        assert_eq!(h.state_mut().progress("job.42", "Rendering 60%", Some(0.6)), job);
        assert_eq!(h.state().get(job).and_then(|t| t.progress), Some(0.6));
        h.step();
        // the cached layout follows the new text
        assert!(automation::find(&h.ctx, &format!("toast.{job}")).is_some_and(|e| e.label == "Rendering 60%"));
        assert_eq!(h.state_mut().progress("job.42", "Rendering", Some(f32::NAN)), job);
        assert_eq!(h.state().get(job).and_then(|t| t.progress), Some(0.0));
        assert_eq!(h.state_mut().push(Toast::success("Rendered").key("job.42")), job);
        let done = h.state().get(job).unwrap();
        assert_eq!((done.text.as_str(), done.progress, done.level), ("Rendered", None, ToastLevel::Success));
        assert_eq!(h.state().len(), 2);
        assert!(h.state_mut().dismiss_key("job.42"));
        assert_eq!(h.state().texts().collect::<Vec<_>>(), ["Saved"]);
    }

    #[test]
    fn actions_are_reachable_by_automation_and_reported() {
        let mut h = harness(Toasts::new());
        let toast = Toast::success("3 files").title("Exported").action("Show", "export.reveal").action("Undo", "edit.undo");
        let id = h.state_mut().push(toast.clone().action("a", "b").action("c", "d").sticky());
        assert_eq!(h.state().get(id).unwrap().actions.len(), MAX_ACTIONS);
        h.state_mut().clear();
        let id = h.state_mut().push(toast.key("export").sticky());
        h.step();
        h.step();
        assert_eq!(automation::find(&h.ctx, &format!("toast.{id}.action.0")).unwrap().label, "Show");
        let undo = automation::find(&h.ctx, &format!("toast.{id}.action.1")).unwrap();
        assert_eq!(undo.role, Role::Button);
        assert_eq!(undo.label, "Undo");
        click(&mut h, &format!("toast.{id}.action.1"));
        assert!(h.state().is_empty(), "an action dismisses its toast");
        let taken = h.state_mut().take_actions();
        assert_eq!(taken, [ToastActivation { toast: id, key: Some("export".into()), index: 1, command: "edit.undo".into() }]);
        assert!(h.state_mut().take_actions().is_empty());
    }

    #[test]
    fn every_toast_is_a_live_region() {
        let mut h = harness(Toasts::new());
        h.state_mut().info("Saved");
        h.state_mut().push(Toast::error("Disk full").title("Export failed").sticky());
        h.step();
        h.step();
        let saved = h.get_by_label("Saved");
        assert_eq!(saved.accesskit_node().live(), Live::Polite);
        assert_eq!(saved.accesskit_node().role(), AkRole::Status);
        let failed = h.get_by_label("Export failed");
        assert_eq!(failed.accesskit_node().live(), Live::Assertive);
        assert_eq!(failed.accesskit_node().role(), AkRole::Alert);
        assert_eq!(failed.accesskit_node().description().as_deref(), Some("Disk full"));
        assert_eq!(h.get_all_by_label("Dismiss").count(), 2);
    }

    #[test]
    fn escape_dismisses_the_newest_but_not_while_a_field_has_focus() {
        let mut h = harness(Toasts::new());
        h.state_mut().push(Toast::info("Older").sticky());
        h.state_mut().push(Toast::info("Newer").sticky());
        h.step();
        h.key_press(Key::Escape);
        h.step();
        assert_eq!(h.state().texts().collect::<Vec<_>>(), ["Older"]);

        // a focused text field keeps its Escape
        let mut h = Harness::builder().with_size(vec2(600.0, 400.0)).build_ui_state(
            |ui, (q, text, focus): &mut (Toasts, String, bool)| {
                let field = ui.text_edit_singleline(text);
                if std::mem::take(focus) {
                    field.request_focus();
                }
                q.show(ui.ctx());
            },
            (Toasts::new(), String::new(), true),
        );
        h.state_mut().0.push(Toast::info("Stays").sticky());
        h.step();
        h.step();
        assert!(h.ctx.memory(|m| m.focused().is_some()));
        h.key_press(Key::Escape);
        h.step();
        assert_eq!(h.state().0.len(), 1);
        // with the field no longer focused, the next Escape reaches the toasts
        h.key_press(Key::Escape);
        h.step();
        assert!(h.state().0.is_empty());
    }

    #[test]
    fn keyboard_focus_pauses_expiry_and_shows_a_ring() {
        let mut h = harness(Toasts::new());
        let id = h.state_mut().push(Toast::info("Saved").duration(Duration::from_secs(1)));
        h.step();
        h.key_press(Key::Tab);
        h.step();
        let close = Id::new(("ashlar_ui::toast", id)).with("close");
        assert!(h.ctx.memory(|m| m.has_focus(close)), "Tab reaches the close button");
        for _ in 0..6 {
            h.step();
        }
        assert_eq!(h.state().len(), 1, "a focused toast does not expire");
        // Escape dismisses the focused toast
        h.key_press(Key::Escape);
        h.step();
        assert!(h.state().is_empty());
    }

    #[test]
    fn overflow_shows_how_many_are_hidden_with_app_labels() {
        let labels = ToastLabels { dismiss: "Schließen".into(), more: "{n} weitere".into() };
        let mut h = harness(Toasts::new().max_visible(2).labels(labels));
        for i in 0..5 {
            h.state_mut().push(Toast::info(format!("n{i}")).sticky());
        }
        h.step();
        h.step();
        assert_eq!(automation::find(&h.ctx, "toast.more").unwrap().label, "3 weitere");
        assert_eq!(automation::find(&h.ctx, "toast.1.close").unwrap().label, "Schließen");
        assert!(automation::find(&h.ctx, "toast.3").is_none());
        h.state_mut().dismiss(1);
        h.step();
        assert_eq!(automation::find(&h.ctx, "toast.more").unwrap().label, "2 weitere");
        // the close target is a full control high
        let close = automation::find(&h.ctx, "toast.2.close").unwrap();
        assert!(close.rect[3] >= tokens::current(&h.ctx).control_height - 0.01, "{close:?}");
    }

    #[test]
    fn a_settled_stack_requests_no_frames() {
        let mut h = harness(Toasts::new().max_visible(1));
        h.state_mut().push(Toast::info("Sticky").sticky().action("Open", "file.open"));
        h.state_mut().progress("job", "Working", Some(0.5));
        h.run();
        h.step();
        let delay = h.output().viewport_output.get(&egui::ViewportId::ROOT).map_or(Duration::MAX, |v| v.repaint_delay);
        assert_eq!(delay, Duration::MAX, "a settled stack of sticky toasts schedules nothing");
    }
    const DT: f64 = 1.0 / 60.0;

    /// One frame at time `t` on a 600 × 400 screen.
    fn frame(ctx: &egui::Context, q: &mut Toasts, t: f64) {
        let input = egui::RawInput {
            time: Some(t),
            predicted_dt: DT as f32,
            screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(600.0, 400.0))),
            ..Default::default()
        };
        ctx.run_ui(input, |ui| q.show(ui.ctx())).textures_delta.clear();
    }

    fn animation_requests(ctx: &egui::Context) -> u64 {
        crate::wake::Scheduler::of(ctx).stats().requests_for(crate::wake::Reason::ANIMATION)
    }

    fn drawn(q: &Toasts, id: u64) -> f32 {
        q.entries.iter().find(|e| e.id == id).and_then(|e| e.drawn).expect("drawn")
    }

    #[test]
    fn a_dismissed_toast_leaves_and_the_rest_glide_into_the_gap() {
        let ctx = egui::Context::default();
        automation::install(&ctx);
        let mut q = Toasts::new();
        let (a, b) = (q.push(Toast::info("first").sticky()), q.push(Toast::info("second").sticky()));
        let mut t = 0.0;
        for _ in 0..3 {
            frame(&ctx, &mut q, t);
            t += DT;
        }
        // nothing animates on first show
        assert_eq!(crate::motion::active(&ctx), 0);
        let rest_a = automation::find(&ctx, &format!("toast.{a}")).expect("a").rect;
        let before = drawn(&q, b);
        assert!(q.dismiss(a));
        assert_eq!(q.len(), 1);
        let mut ys = vec![before];
        let mut gone_at = None;
        for k in 1..120 {
            frame(&ctx, &mut q, t);
            t += DT;
            ys.push(drawn(&q, b));
            if k == 3 {
                // 50 ms in: still painted, never interactive or reported
                assert!(q.leaving.iter().any(|e| e.id == a), "the dismissed toast vanished at once");
                assert!(automation::find(&ctx, &format!("toast.{a}")).is_none());
                // the survivor reports its resting place, not where it is drawn
                let rest_b = automation::find(&ctx, &format!("toast.{b}")).expect("b").rect;
                assert_eq!(rest_b, rest_a);
                assert_ne!(ys[k], 0.0, "the survivor jumped into the gap");
            }
            if q.leaving.is_empty() && gone_at.is_none() {
                gone_at = Some(k);
            }
        }
        assert!(gone_at.is_some_and(|k| k > 3), "left at {gone_at:?}");
        // the survivor moves one way only, and lands exactly
        assert!(ys.windows(2).all(|w| w[1] >= w[0]), "{ys:?}");
        assert_eq!(*ys.last().unwrap(), 0.0);
        // settled: no more frames
        let flat = animation_requests(&ctx);
        for _ in 0..100 {
            frame(&ctx, &mut q, t);
            t += DT;
        }
        assert_eq!(animation_requests(&ctx), flat);
    }

    #[test]
    fn a_new_toast_fades_in_after_first_show_and_reduced_motion_snaps() {
        let ctx = egui::Context::default();
        let mut q = Toasts::new();
        frame(&ctx, &mut q, 0.0);
        q.info("hello");
        frame(&ctx, &mut q, DT);
        assert_eq!(crate::motion::active(&ctx), 1, "a new toast animates in");

        // reduced motion: the gap closes at once, the leaving toast only fades
        let ctx = egui::Context::default();
        crate::motion::set_prefs(&ctx, ashlar_motion::MotionPrefs::REDUCED);
        let mut q = Toasts::new();
        let (a, b) = (q.push(Toast::info("first").sticky()), q.push(Toast::info("second").sticky()));
        let mut t = 0.0;
        for _ in 0..3 {
            frame(&ctx, &mut q, t);
            t += DT;
        }
        q.dismiss(a);
        frame(&ctx, &mut q, t);
        assert_eq!(drawn(&q, b), 0.0);
        assert!(q.leaving.iter().any(|e| e.id == a));
        for _ in 0..30 {
            t += DT;
            frame(&ctx, &mut q, t);
        }
        assert!(q.leaving.is_empty());
    }
}
