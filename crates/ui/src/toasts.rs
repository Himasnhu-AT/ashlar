//! Toast notifications: a bounded queue the app pushes to, painted in a corner.
//!
//! Timed toasts disappear after their duration (the timer pauses while the pointer is over them);
//! sticky ones stay until dismissed. Clicking a toast's close button dismisses it. An empty queue
//! costs nothing per frame; a visible timed toast schedules exactly one repaint at its expiry.

use std::collections::VecDeque;
use std::time::Duration;

use egui::{Align2, Id, Order, Rect, Sense, Stroke, StrokeKind, Ui, Vec2, pos2, text::LayoutJob, vec2};

use crate::automation::{self, Details, Role};
use crate::tokens::{self, Tokens};

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

/// One notification.
#[derive(Clone, Debug, PartialEq)]
pub struct Toast {
    pub level: ToastLevel,
    pub text: String,
    /// `None`: stays until dismissed.
    pub duration: Option<Duration>,
}

impl Toast {
    pub fn new(level: ToastLevel, text: impl Into<String>) -> Self {
        let duration = Some(if level == ToastLevel::Error { Duration::from_secs(8) } else { Duration::from_secs(4) });
        Toast { level, text: text.into(), duration }
    }
    pub fn sticky(mut self) -> Self {
        self.duration = None;
        self
    }
    pub fn duration(mut self, d: Duration) -> Self {
        self.duration = Some(d);
        self
    }
}

struct Entry {
    id: u64,
    toast: Toast,
    /// Seconds of display time left (timed toasts only).
    remaining: Option<f64>,
}

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
}

impl Default for Toasts {
    fn default() -> Self {
        Self::new()
    }
}

/// Longest toast text kept (longer text is cut, so a runaway message cannot fill memory).
pub const MAX_TEXT: usize = 2000;

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
        }
    }
    /// Keep at most `capacity` toasts (the oldest are dropped first).
    pub fn capacity(mut self, capacity: usize) -> Self {
        self.capacity = capacity.clamp(1, 1024);
        self
    }
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

    /// Queue a toast; returns its id (for [`Toasts::dismiss`]).
    pub fn push(&mut self, mut toast: Toast) -> u64 {
        if toast.text.len() > MAX_TEXT {
            let mut cut = MAX_TEXT;
            while !toast.text.is_char_boundary(cut) {
                cut -= 1;
            }
            toast.text.truncate(cut);
            toast.text.push('…');
        }
        while self.entries.len() >= self.capacity {
            self.entries.pop_front();
        }
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        let remaining = toast.duration.map(|d| d.as_secs_f64().min(3600.0));
        self.entries.push_back(Entry { id, toast, remaining });
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
    pub fn dismiss(&mut self, id: u64) -> bool {
        let before = self.entries.len();
        self.entries.retain(|e| e.id != id);
        before != self.entries.len()
    }
    pub fn clear(&mut self) {
        self.entries.clear();
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

    /// Paint the visible toasts and age them. Call once per frame, after the main UI.
    pub fn show(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        let dt = self.last_time.map_or(0.0, |last| (now - last).clamp(0.0, 1.0));
        self.last_time = Some(now);
        if self.entries.is_empty() {
            self.last_time = None;
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
        let mut offset = 0.0f32;
        let mut dismiss = None;
        let mut next_expiry: Option<f64> = None;
        let visible = self.entries.len().min(self.max_visible);
        for entry in self.entries.iter_mut().take(visible) {
            let area_id = Id::new(("ashlar_ui::toast", entry.id));
            let y = if down { offset } else { -offset };
            let x = if matches!(anchor, Align2::RIGHT_TOP | Align2::RIGHT_BOTTOM) { -margin - self.inset.x } else { margin + self.inset.x };
            let y_margin = if down { margin + self.inset.y } else { -margin - self.inset.y };
            let area = egui::Area::new(area_id).order(Order::Foreground).anchor(anchor, vec2(x, y + y_margin)).interactable(true);
            let mut hovered = false;
            let inner = area.show(ctx, |ui| {
                ui.set_width(width);
                let (closed, h) = paint_toast(ui, &t, entry);
                hovered = ui.rect_contains_pointer(ui.min_rect());
                (closed, h)
            });
            let (closed, height) = inner.inner;
            offset += height + t.space.sm;
            if closed {
                dismiss = Some(entry.id);
            }
            if let Some(rem) = entry.remaining.as_mut() {
                if !hovered {
                    *rem -= dt;
                }
                if *rem <= 0.0 {
                    dismiss.get_or_insert(entry.id);
                } else {
                    next_expiry = Some(next_expiry.map_or(*rem, |n: f64| n.min(*rem)));
                }
            }
        }
        self.entries.retain(|e| Some(e.id) != dismiss && e.remaining.is_none_or(|r| r > 0.0));
        if dismiss.is_some() {
            crate::wake::wake(ctx, crate::wake::Reason::INPUT);
        } else if let Some(secs) = next_expiry {
            crate::wake::wake_after(ctx, crate::wake::Reason::TIMER, Duration::from_secs_f64(secs.max(0.016)));
        }
    }
}

/// Returns (close clicked, height).
fn paint_toast(ui: &mut Ui, t: &Tokens, entry: &Entry) -> (bool, f32) {
    let accent = match entry.toast.level {
        ToastLevel::Info => t.status.info,
        ToastLevel::Success => t.status.success,
        ToastLevel::Warning => t.status.warning,
        ToastLevel::Error => t.status.danger,
    };
    let width = ui.available_width();
    let pad = t.space.md;
    let close_w = t.control_height;
    let text_w = (width - 2.0 * pad - close_w - 6.0).max(40.0);
    let font = Tokens::font(t.type_scale.body);
    let revision = crate::cache::hash_of(&(text_w.to_bits(), font.size.to_bits(), t.text.primary));
    let galley = crate::cache::galley(ui, ("ashlar_ui::toast", entry.id), revision, || {
        LayoutJob::simple(entry.toast.text.clone(), font, t.text.primary, text_w)
    });
    let height = (galley.size().y + 2.0 * pad).max(t.control_height + pad);
    let (rect, resp) = ui.allocate_exact_size(vec2(width, height), Sense::click());
    let painter = ui.painter();
    painter.add(egui::Shape::Rect(egui::epaint::RectShape::filled(rect, t.radius.lg, t.surface.raised)));
    painter.rect_stroke(rect, t.radius.lg, Stroke::new(1.0, t.border), StrokeKind::Inside);
    let bar = Rect::from_min_size(rect.min, vec2(4.0, rect.height()));
    painter.rect_filled(bar, egui::CornerRadius { nw: t.radius.lg, sw: t.radius.lg, ne: 0, se: 0 }, accent);
    painter.galley(pos2(rect.left() + pad + 6.0, rect.top() + pad), galley, t.text.primary);
    let close_rect = Rect::from_center_size(pos2(rect.right() - pad - close_w / 2.0 + 4.0, rect.top() + pad + 8.0), Vec2::splat(close_w * 0.8));
    let close = ui.interact(close_rect, resp.id.with("close"), Sense::click());
    let c = close_rect.center();
    let k = close_rect.width() * 0.22;
    let col = if close.hovered() { t.text.primary } else { t.text.secondary };
    if close.hovered() {
        ui.painter().rect_filled(close_rect, t.radius.sm, t.surface.hover);
    }
    ui.painter().line_segment([c - vec2(k, k), c + vec2(k, k)], Stroke::new(1.5, col));
    ui.painter().line_segment([c + vec2(-k, k), c + vec2(k, -k)], Stroke::new(1.5, col));
    let id = entry.id;
    let level = level_name(entry.toast.level);
    automation::register_fmt(
        ui,
        rect,
        format_args!("toast.{id}"),
        &entry.toast.text,
        Role::Toast,
        true,
        Details { value: Some(level), selected: None },
    );
    automation::register_fmt(ui, close_rect, format_args!("toast.{id}.close"), "Dismiss", Role::Button, true, Details::default());
    close.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Dismiss"));
    (close.clicked(), height)
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
}
