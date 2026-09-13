//! An optional egui renderer for a [`Layout`]: tab strips, accordions, split and accordion
//! dividers, floating groups, drag and drop. It never changes the layout; everything the user
//! asks for comes back as [`Action`]s in a [`DockOutput`].

use std::collections::HashSet;
use std::fmt::Debug;
use std::hash::Hash;

use egui::{
    Align, Align2, Button, Color32, Context, CursorIcon, EventFilter, FontId, Id, Key, LayerId, Modifiers, Order, Pos2, Rect, Response, RichText,
    ScrollArea, Sense, Stroke, StrokeKind, TextWrapMode, Ui, UiBuilder, Vec2, WidgetInfo, WidgetText, WidgetType, pos2, vec2,
};

use super::model::{Action, DockError, Layout, MAX_PANELS, Node, Permissions, Placement, StackEntry};
use super::targets::{DropProposal, DropTarget, resolve_drop};
use crate::layout::{EXTENT, ResizeHandle, ResizeHandleStyle, SplitAxis, SplitSize, split_rect, usable};
use crate::tabs::{Tab, fit};

/// Most interaction ids recorded per frame.
const MAX_OWNED: usize = 8 * MAX_PANELS;
/// Width of a floating group's move grip.
const GRIP: f32 = 12.0;
/// Side of a floating group's resize corner.
const CORNER: f32 = 12.0;
/// Width of an accordion entry's collapse button.
const COLLAPSE: f32 = 18.0;
/// Horizontal padding added to a tab's label width.
const TAB_PADDING: f32 = 24.0;
/// Narrowest a tab is squeezed to before tabs overflow.
const TAB_MIN: f32 = 40.0;
/// Width of the overflow button.
const OVERFLOW: f32 = 22.0;
/// Distance a floated panel is offset from its group by the context menu.
const FLOAT_OFFSET: f32 = 24.0;
/// Arrow-key step of the floating resize corner.
const KEY_STEP: f32 = 8.0;
/// Pointer travel below which a released tab drag does nothing.
const CLICK_SLOP: f32 = 2.0;
/// Most points a style dimension may have.
const STYLE_MAX: f32 = 4096.0;

/// Colours, metrics, font and (localizable) labels of the renderer.
#[derive(Clone, Debug)]
pub struct DockStyle {
    pub tab_height: f32,
    pub gap: f32,
    pub min_pane: f32,
    pub background: Color32,
    pub tab_background: Color32,
    pub active_background: Color32,
    pub text: Color32,
    pub inactive_text: Color32,
    pub border: Stroke,
    pub accent: Color32,
    pub font: FontId,
    pub float_label: String,
    pub close_label: String,
    pub move_label: String,
    pub panels_label: String,
    pub resize_label: String,
    pub resize_window_label: String,
}

impl DockStyle {
    /// Defaults taken from the current egui visuals, with English labels.
    pub fn from_ui(ui: &Ui) -> Self {
        let v = ui.visuals();
        Self {
            tab_height: 26.0,
            gap: 4.0,
            min_pane: 40.0,
            background: v.panel_fill,
            tab_background: v.extreme_bg_color,
            active_background: v.widgets.active.bg_fill,
            text: v.text_color(),
            inactive_text: v.weak_text_color(),
            border: v.widgets.noninteractive.bg_stroke,
            accent: v.selection.stroke.color,
            font: FontId::proportional(12.0),
            float_label: "Float panel".to_owned(),
            close_label: "Close panel".to_owned(),
            move_label: "Move to group".to_owned(),
            panels_label: "Panels".to_owned(),
            resize_label: "Resize panels".to_owned(),
            resize_window_label: "Resize panel window".to_owned(),
        }
    }

    fn valid(&self) -> bool {
        let dim = |v: f32| v.is_finite() && (0.0..=STYLE_MAX).contains(&v);
        dim(self.tab_height)
            && dim(self.gap)
            && dim(self.min_pane)
            && dim(self.border.width)
            && dim(self.font.size)
            && self.tab_height > 0.0
            && self.font.size > 0.0
    }
}

/// What the user asked for this frame.
#[derive(Clone, Debug)]
pub struct DockOutput<P> {
    /// Changes to apply, in the order they were made.
    pub actions: Vec<Action<P>>,
    /// Why nothing was rendered, if the layout or style was rejected.
    pub error: Option<DockError>,
    /// The panel the user interacted with (clicked, raised, activated, dropped).
    pub focused: Option<P>,
    /// The floating group (by first panel) being moved by its grip, while the button is held.
    pub group_drag: Option<P>,
    /// The floating group (by first panel) whose grip was released, and where.
    pub group_drop: Option<(P, Pos2)>,
}

impl<P> Default for DockOutput<P> {
    fn default() -> Self {
        Self { actions: Vec::new(), error: None, focused: None, group_drag: None, group_drop: None }
    }
}

/// Per-panel size limits (points), applied by the renderer without changing saved sizes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PanelLimits {
    pub min: Vec2,
    pub max: Vec2,
}

impl Default for PanelLimits {
    fn default() -> Self {
        Self { min: Vec2::ZERO, max: Vec2::splat(EXTENT) }
    }
}

impl PanelLimits {
    fn valid(self) -> bool {
        self.min.is_finite()
            && self.max.is_finite()
            && self.min.x >= 0.0
            && self.min.y >= 0.0
            && self.max.x >= self.min.x
            && self.max.y >= self.min.y
            && self.max.x <= EXTENT
            && self.max.y <= EXTENT
    }
}

/// Hooks for apps that draw their own panel contents, labels, menus and group headers.
pub trait DockContent<P> {
    /// Draw the body of `panel`.
    fn body(&mut self, ui: &mut Ui, panel: &P);

    /// The visible tab text for `panel`; `logical` stays its accessible name.
    fn label(&mut self, _ui: &Ui, _panel: &P, logical: &str, font: &FontId, color: Color32) -> WidgetText {
        RichText::new(logical).font(font.clone()).color(color).into()
    }

    /// The visible text of a stock menu action (float, move, close); `logical` stays its
    /// accessible name.
    fn action_label(&mut self, _ui: &Ui, logical: &str, font: &FontId, color: Color32) -> WidgetText {
        RichText::new(logical).font(font.clone()).color(color).into()
    }

    /// Extra entries at the top of a tab's context menu.
    fn panel_menu(&mut self, _ui: &mut Ui, _panel: &P) {}

    /// Width to reserve at the right end of a group's tab strip for [`Self::group_header`].
    fn group_header_width(&self, _panels: &[P], _active: &P, _floating: bool) -> f32 {
        0.0
    }

    /// Draw into the reserved header strip (clipped to it).
    fn group_header(&mut self, _ui: &mut Ui, _panels: &[P], _active: &P, _floating: bool) {}
}

/// [`DockContent`] for the closure-based entry points.
struct BodyFn<F>(F);

impl<P, F: FnMut(&mut Ui, &P)> DockContent<P> for BodyFn<F> {
    fn body(&mut self, ui: &mut Ui, panel: &P) {
        (self.0)(ui, panel);
    }
}

/// A tab drag in progress (context temp data, keyed by the area).
#[derive(Clone)]
struct TabDrag<P> {
    panel: P,
    origin: Pos2,
    source: Rect,
    grab: Vec2,
}

/// A floating group being moved by its grip.
#[derive(Clone)]
struct GroupDrag<P> {
    first: P,
    original: Rect,
    origin: Pos2,
}

fn drag_key(area: Id) -> Id {
    area.with("drag")
}

fn group_drag_key(area: Id) -> Id {
    area.with("group-drag")
}

fn owned_key(area: Id) -> Id {
    area.with("owned-interactions")
}

fn tab_id<P: Hash + Debug>(area: Id, panel: &P) -> Id {
    area.with(("tab", panel))
}

fn floating_id<P: Hash + Debug>(area: Id, first: &P) -> Id {
    area.with(("floating", first))
}

fn grip_id<P: Hash + Debug>(area: Id, first: Option<&P>) -> Id {
    area.with(("move-group", first))
}

fn overflow_id<P: Hash + Debug>(area: Id, selected: &P) -> Id {
    area.with(("overflow", selected))
}

fn to_array(r: Rect) -> [f32; 4] {
    [r.min.x, r.min.y, r.width(), r.height()]
}

fn from_array(r: [f32; 4]) -> Rect {
    Rect::from_min_size(pos2(r[0], r[1]), vec2(r[2], r[3]))
}

/// Fit `rect` on `screen`: at least 1×1, at most the screen size, fully inside.
fn clamp_to_screen(rect: Rect, screen: Rect) -> Rect {
    if !usable(screen) || !rect.min.is_finite() || !rect.size().is_finite() {
        return rect;
    }
    let size = rect.size().max(Vec2::splat(1.0)).min(screen.size());
    let max_min = screen.max - size;
    let min = pos2(rect.min.x.max(screen.min.x).min(max_min.x), rect.min.y.max(screen.min.y).min(max_min.y));
    Rect::from_min_size(min, size)
}

/// Bounds for the first of two panes sharing `space`, given their minimums and maximums.
fn pair_bounds(space: f32, (a, b): (f32, f32), (a_max, b_max): (f32, f32)) -> (f32, f32) {
    let s = if a + b > space { space / (a + b).max(1.0) } else { 1.0 };
    let lo = (a * s).min(space);
    let hi = (space - b * s).max(lo).min(space);
    let (lo2, hi2) = (lo.max(space - b_max), hi.min(a_max));
    if lo2 <= hi2 { (lo2, hi2) } else { (lo, hi) }
}

/// Start dragging `panel` out of a group at `source_rect`, as a tab press would. Custom headers
/// use this to hand their gesture to the renderer of the same area. False when there is no
/// pointer position or the rect is unusable.
pub fn begin_drag<P: Clone + Send + Sync + 'static>(ctx: &Context, area_id: Id, panel: P, source_rect: Rect) -> bool {
    let (pointer, press) = ctx.input(|i| (i.pointer.interact_pos(), i.pointer.press_origin()));
    let Some(pointer) = pointer.filter(|p| p.is_finite()) else { return false };
    if !usable(source_rect) {
        return false;
    }
    let origin = press.filter(|p| p.is_finite()).unwrap_or(pointer);
    let grab = (pointer - source_rect.min).max(Vec2::ZERO).min(source_rect.size());
    ctx.data_mut(|d| d.insert_temp(drag_key(area_id), TabDrag { panel, origin, source: source_rect, grab }));
    true
}

/// Abandon any tab or group drag of this area, and stop egui's drag if it belongs to one of the
/// area's controls (tabs, dividers, grips, resize corners). Never touches focus.
pub fn cancel_drag<P: Clone + Send + Sync + 'static>(ctx: &Context, area_id: Id) {
    let owned = ctx.data_mut(|d| {
        d.remove::<TabDrag<P>>(drag_key(area_id));
        d.remove::<GroupDrag<P>>(group_drag_key(area_id));
        d.remove_temp::<Vec<Id>>(owned_key(area_id))
    });
    if let (Some(owned), Some(dragged)) = (owned, ctx.dragged_id())
        && owned.contains(&dragged)
    {
        ctx.stop_dragging();
    }
}

/// The docking renderer for one area of the screen.
pub struct DockArea<P = ()> {
    id: Id,
    raise: Option<P>,
    blockers: Vec<Rect>,
}

impl<P: Clone + Eq + Hash + Debug + Send + Sync + 'static> DockArea<P> {
    pub fn new(id: Id) -> Self {
        Self { id, raise: None, blockers: Vec::new() }
    }

    /// Whether keyboard focus is on this area's own chrome (tabs, overflow buttons, dividers,
    /// collapse buttons, grips, resize corners) as rendered last frame. Panel contents and custom
    /// header or menu widgets never count.
    pub fn chrome_has_focus(&self, ctx: &Context, layout: &Layout<P>) -> bool {
        let Some(focused) = ctx.memory(|m| m.focused()) else { return false };
        if layout.validate().is_err() || ctx.read_response(focused).is_none() {
            return false;
        }
        let area = self.id;
        let panels = layout.panels();
        let per_panel = panels
            .iter()
            .any(|p| [tab_id(area, *p), overflow_id(area, *p), area.with(("collapse", *p)), area.with(("stack-resize", *p))].contains(&focused));
        if per_panel {
            return true;
        }
        let floating = layout
            .floating
            .iter()
            .filter_map(|g| g.panels.first())
            .any(|f| grip_id(area, Some(f)) == focused || floating_id(area, f).with("resize") == focused);
        if floating {
            return true;
        }
        let mut stack: Vec<(&Node<P>, Vec<bool>)> = layout.root.iter().map(|n| (n, Vec::new())).collect();
        while let Some((node, path)) = stack.pop() {
            if let Node::Split { first, second, .. } = node {
                if area.with(("split", &path)) == focused {
                    return true;
                }
                let mut b = path.clone();
                b.push(true);
                stack.push((second, b));
                let mut a = path;
                a.push(false);
                stack.push((first, a));
            }
        }
        false
    }

    /// Raise the floating group holding `panel` this frame and focus its tab.
    pub fn raise_panel(mut self, panel: P) -> Self {
        self.raise = Some(panel);
        self
    }

    /// Screen areas (menus, dialogs, overlays) where drops, previews and floating are blocked.
    pub fn blocked_rects(mut self, rects: &[Rect]) -> Self {
        self.blockers = rects.to_vec();
        self
    }

    pub fn show(
        self,
        ui: &mut Ui,
        layout: &Layout<P>,
        style: &DockStyle,
        label: impl Fn(&P) -> String,
        permissions: impl Fn(&P) -> Permissions,
        body: impl FnMut(&mut Ui, &P),
    ) -> DockOutput<P> {
        self.show_with_limits(ui, layout, style, label, permissions, |_| PanelLimits::default(), body)
    }

    pub fn show_with_limits(
        self,
        ui: &mut Ui,
        layout: &Layout<P>,
        style: &DockStyle,
        label: impl Fn(&P) -> String,
        permissions: impl Fn(&P) -> Permissions,
        limits: impl Fn(&P) -> PanelLimits,
        body: impl FnMut(&mut Ui, &P),
    ) -> DockOutput<P> {
        self.show_customized(ui, layout, style, label, permissions, limits, &mut BodyFn(body))
    }

    pub fn show_customized(
        self,
        ui: &mut Ui,
        layout: &Layout<P>,
        style: &DockStyle,
        label: impl Fn(&P) -> String,
        permissions: impl Fn(&P) -> Permissions,
        limits: impl Fn(&P) -> PanelLimits,
        content: &mut impl DockContent<P>,
    ) -> DockOutput<P> {
        let ctx = ui.ctx().clone();
        let area = ui.available_rect_before_wrap();
        if let Err(error) = layout.validate() {
            cancel_drag::<P>(&ctx, self.id);
            return DockOutput { error: Some(error), ..DockOutput::default() };
        }
        if !style.valid() || !usable(area) || layout.panels().iter().any(|p| !limits(p).valid()) {
            cancel_drag::<P>(&ctx, self.id);
            return DockOutput { error: Some(DockError::InvalidGeometry), ..DockOutput::default() };
        }

        // Focus loss shows up as the raw flag or as an event this frame (hosts differ).
        let (escape, unfocused) =
            ctx.input(|i| (i.key_pressed(Key::Escape), !i.focused || i.events.iter().any(|e| matches!(e, egui::Event::WindowFocused(false)))));
        let previous: HashSet<Id> = ctx.data(|d| d.get_temp::<Vec<Id>>(owned_key(self.id))).unwrap_or_default().into_iter().collect();
        let mut r = Renderer {
            ctx: ctx.clone(),
            id: self.id,
            style,
            layout,
            label: &label,
            permissions: &permissions,
            limits: &limits,
            content,
            out: DockOutput::default(),
            targets: Vec::new(),
            owned: Vec::new(),
            owned_set: HashSet::new(),
            previous,
            screen: ctx.content_rect(),
            raise: self.raise,
            blockers: self.blockers,
            escape,
            unfocused,
        };

        if let Some(root) = &layout.root {
            let mut path = Vec::new();
            r.node(ui, root, area, &mut path);
            ui.allocate_rect(area, Sense::hover());
        }
        r.floating_groups();
        r.finish_group_drag();
        r.finish_tab_drag();
        let owned = std::mem::take(&mut r.owned);
        ctx.data_mut(|d| d.insert_temp(owned_key(self.id), owned));
        r.out
    }
}

/// One frame of rendering.
struct Renderer<'a, P> {
    ctx: Context,
    id: Id,
    style: &'a DockStyle,
    layout: &'a Layout<P>,
    label: &'a dyn Fn(&P) -> String,
    permissions: &'a dyn Fn(&P) -> Permissions,
    limits: &'a dyn Fn(&P) -> PanelLimits,
    content: &'a mut dyn DockContent<P>,
    out: DockOutput<P>,
    targets: Vec<DropTarget<P>>,
    owned: Vec<Id>,
    owned_set: HashSet<Id>,
    /// Ids already in the stored list from last frame.
    previous: HashSet<Id>,
    screen: Rect,
    raise: Option<P>,
    blockers: Vec<Rect>,
    escape: bool,
    unfocused: bool,
}

/// Which kind of group is being drawn.
#[derive(Clone, Copy, PartialEq)]
enum Host {
    Docked,
    /// A floating group with its current (clamped) rect.
    Floating(Rect),
}

impl<P: Clone + Eq + Hash + Debug + Send + Sync + 'static> Renderer<'_, P> {
    /// Remember that `id` is one of this area's controls, and publish it right away so that
    /// `cancel_drag` called from a callback later this frame sees it.
    fn own(&mut self, id: Id) {
        if self.owned.len() >= MAX_OWNED || !self.owned_set.insert(id) {
            return;
        }
        self.owned.push(id);
        if !self.previous.contains(&id) {
            let key = owned_key(self.id);
            self.ctx.data_mut(|d| {
                let list = d.get_temp_mut_or_default::<Vec<Id>>(key);
                if list.len() < 2 * MAX_OWNED {
                    list.push(id);
                }
            });
        }
    }

    fn blocked(&self, p: Pos2) -> bool {
        self.blockers.iter().any(|b| b.contains(p))
    }

    fn movable(&self, p: &P) -> bool {
        (self.permissions)(p).movable
    }

    fn protected_single(&self, panels: &[P]) -> bool {
        matches!(panels, [only] if (self.permissions)(only) == Permissions::PROTECTED)
    }

    // -----------------------------------------------------------------------------------------
    // Limits

    fn group_limits(&self, panels: &[P]) -> PanelLimits {
        let mut min = Vec2::splat(self.style.min_pane);
        let mut max = Vec2::splat(EXTENT);
        for p in panels {
            let l = (self.limits)(p);
            min = min.max(l.min);
            max = max.min(l.max);
        }
        max = max.max(min);
        if !self.protected_single(panels) {
            min.y = (min.y + self.style.tab_height).min(EXTENT);
            max.y = (max.y + self.style.tab_height).min(EXTENT);
        }
        PanelLimits { min, max }
    }

    fn stack_limits(&self, entries: &[StackEntry<P>]) -> PanelLimits {
        let pane = self.style.min_pane;
        let mut min = vec2(pane, 0.0);
        let mut max_x = EXTENT;
        for e in entries {
            let l = (self.limits)(&e.panel);
            min.x = min.x.max(l.min.x);
            max_x = max_x.min(l.max.x);
            let body = if e.open { l.min.y.max(pane) } else { 0.0 };
            min.y += body + self.style.tab_height;
        }
        min = min.min(Vec2::splat(EXTENT));
        PanelLimits { min, max: vec2(max_x.max(min.x), EXTENT) }
    }

    /// Aggregated limits of a subtree; recursion is bounded by the validated depth.
    fn node_limits(&self, node: &Node<P>) -> PanelLimits {
        match node {
            Node::Tabs { panels, .. } => self.group_limits(panels),
            Node::Stack { entries } => self.stack_limits(entries),
            Node::Split { axis, first, second, .. } => {
                let (a, b) = (self.node_limits(first), self.node_limits(second));
                let gap = self.style.gap;
                let (mut min, mut max) = match axis {
                    SplitAxis::Horizontal => {
                        (vec2(a.min.x + b.min.x + gap, a.min.y.max(b.min.y)), vec2(a.max.x + b.max.x + gap, a.max.y.min(b.max.y)))
                    }
                    SplitAxis::Vertical => (vec2(a.min.x.max(b.min.x), a.min.y + b.min.y + gap), vec2(a.max.x.min(b.max.x), a.max.y + b.max.y + gap)),
                };
                min = min.min(Vec2::splat(EXTENT));
                max = max.min(Vec2::splat(EXTENT)).max(min);
                PanelLimits { min, max }
            }
        }
    }

    // -----------------------------------------------------------------------------------------
    // Docked tree

    fn node(&mut self, ui: &mut Ui, node: &Node<P>, rect: Rect, path: &mut Vec<bool>) {
        match node {
            Node::Split { axis, size, first, second } => self.split(ui, *axis, *size, first, second, rect, path),
            Node::Tabs { panels, active } => self.group(ui, panels, *active, rect, Host::Docked),
            Node::Stack { entries } => self.stack(ui, entries, rect),
        }
    }

    fn split(&mut self, ui: &mut Ui, axis: SplitAxis, size: SplitSize, first: &Node<P>, second: &Node<P>, rect: Rect, path: &mut Vec<bool>) {
        let gap = self.style.gap;
        let Some(base) = split_rect(rect, axis, size, gap, 0.0) else { return };
        let (a, b) = (self.node_limits(first), self.node_limits(second));
        let space = base.available();
        let (lo, hi) = pair_bounds(space, (axis.along(a.min), axis.along(b.min)), (axis.along(a.max), axis.along(b.max)));
        let first_size = base.first_size().max(lo).min(hi);
        let Some(laid) = split_rect(rect, axis, SplitSize::FixedFirst(first_size), gap, 0.0) else { return };

        path.push(false);
        self.node(ui, first, laid.first, path);
        path.pop();
        path.push(true);
        self.node(ui, second, laid.second, path);
        path.pop();

        // After the children, so the divider's grab margin wins over their scrollbars.
        let id = self.id.with(("split", &*path));
        self.own(id);
        let accent = self.style.accent;
        let handle = ResizeHandle::new(
            id,
            axis,
            laid.divider,
            &self.style.resize_label,
            ResizeHandleStyle { hovered: accent, dragged: accent, focused: accent },
        )
        .value(first_size, lo..=hi)
        .show(ui);
        if handle.delta != 0.0 {
            let wanted = (first_size + handle.delta).max(lo).min(hi);
            if let Some(size) = laid.resized(size, wanted - first_size, 0.0) {
                self.out.actions.push(Action::ResizeSplit { path: path.clone(), size });
            }
        }
    }

    fn stack(&mut self, ui: &mut Ui, entries: &[StackEntry<P>], rect: Rect) {
        let n = entries.len();
        if n == 0 {
            return;
        }
        let th = self.style.tab_height;
        let header = th.min(rect.height() / n as f32);
        let remaining = (rect.height() - (n as f32 * th).min(rect.height())).max(0.0);
        let weight = |e: &StackEntry<P>| if e.open { e.height.unwrap_or(self.style.min_pane).max(1.0) } else { 0.0 };
        let total: f32 = entries.iter().map(weight).sum();
        let per_unit = if total > 0.0 { remaining / total } else { 0.0 };

        // Layout pass: each entry's panel rect (title + body share).
        let mut rows: Vec<(Rect, Rect)> = Vec::with_capacity(n);
        let mut y = rect.top();
        for e in entries {
            let share = weight(e) * per_unit;
            let bottom = (y + header + share).min(rect.bottom());
            let panel = Rect::from_min_max(pos2(rect.left(), y), pos2(rect.right(), bottom.max(y)));
            let title = Rect::from_min_max(panel.min, pos2(panel.right(), (y + header).min(panel.bottom())));
            rows.push((panel, title));
            y = panel.bottom();
        }

        let font = self.style.font.clone();
        for (e, &(panel_rect, title)) in entries.iter().zip(&rows) {
            let p = &e.panel;
            let logical = (self.label)(p);
            let color = if e.open { self.style.text } else { self.style.inactive_text };
            let galley = self.content.label(ui, p, &logical, &font, color).into_galley(ui, Some(TextWrapMode::Extend), f32::INFINITY, font.clone());
            self.tab(ui, p, title, e.open, panel_rect, galley, color, &logical);

            // The collapse button, after the tab so it wins where they overlap.
            let button = Rect::from_min_max(pos2((title.right() - COLLAPSE).max(title.left()), title.top()), title.max);
            let cid = self.id.with(("collapse", p));
            if usable(button) {
                let r = ui.interact(button, cid, Sense::click());
                let enabled = ui.is_enabled();
                r.widget_info(|| WidgetInfo::selected(WidgetType::Checkbox, enabled, e.open, &logical));
                let glyph = if e.open { "\u{2212}" } else { "+" };
                ui.painter().with_clip_rect(button.intersect(ui.clip_rect())).text(
                    button.center(),
                    Align2::CENTER_CENTER,
                    glyph,
                    font.clone(),
                    self.style.text,
                );
                if r.clicked() {
                    self.out.actions.push(Action::SetStackOpen { panel: p.clone(), open: !e.open });
                }
            }

            if e.open {
                let body = Rect::from_min_max(pos2(panel_rect.left(), title.bottom()), panel_rect.max);
                if usable(body) {
                    self.body(ui, p, body);
                }
            }
            let accepts = (self.permissions)(p).accepts_tabs;
            self.targets.push(DropTarget {
                anchor: p.clone(),
                rect: panel_rect,
                tab_strip: Some(title),
                tabs: vec![(p.clone(), title)],
                allow_splits: true,
                accepts_tabs: accepts,
            });
        }

        // Dividers after every header, each pairing an open entry with the next open one below.
        if remaining <= 0.0 || per_unit <= 0.0 {
            return;
        }
        let open: Vec<usize> = (0..n).filter(|&i| entries.get(i).is_some_and(|e| e.open)).collect();
        for pair in open.windows(2) {
            let (Some(&ui_index), Some(&li)) = (pair.first(), pair.get(1)) else { continue };
            let (Some(upper), Some(lower), Some(&(upper_rect, _))) = (entries.get(ui_index), entries.get(li), rows.get(ui_index)) else { continue };
            let (wu, wl) = (weight(upper), weight(lower));
            let (su, sl) = (wu * per_unit, wl * per_unit);
            let pane = self.style.min_pane;
            let (lu, ll) = ((self.limits)(&upper.panel), (self.limits)(&lower.panel));
            let (lo, hi) = pair_bounds(su + sl, (lu.min.y.max(pane), ll.min.y.max(pane)), (lu.max.y.max(pane), ll.max.y.max(pane)));
            let y = upper_rect.bottom();
            let divider = Rect::from_min_max(pos2(rect.left(), y - 1.0), pos2(rect.right(), y + 1.0)).intersect(rect);
            let id = self.id.with(("stack-resize", &upper.panel));
            self.own(id);
            let accent = self.style.accent;
            let handle = ResizeHandle::new(
                id,
                SplitAxis::Vertical,
                divider,
                &self.style.resize_label,
                ResizeHandleStyle { hovered: accent, dragged: accent, focused: accent },
            )
            .value(su, lo.min(hi)..=hi)
            .show(ui);
            if handle.delta != 0.0 {
                let wanted_px = (su + handle.delta).max(lo).min(hi);
                let sum = wu + wl;
                let new = (wanted_px / per_unit).max((sum - EXTENT).max(1.0)).min((sum - 1.0).min(EXTENT));
                if new.is_finite() {
                    self.out.actions.push(Action::ResizeStack { panel: upper.panel.clone(), height: Some(new) });
                    self.out.actions.push(Action::ResizeStack { panel: lower.panel.clone(), height: Some(sum - new) });
                }
            }
        }
    }

    // -----------------------------------------------------------------------------------------
    // Groups

    fn group(&mut self, ui: &mut Ui, panels: &[P], active: usize, rect: Rect, host: Host) {
        let Some(selected) = panels.get(active) else { return };
        let floating = matches!(host, Host::Floating(_));
        if self.protected_single(panels) {
            self.body(ui, selected, rect);
            self.targets.push(DropTarget {
                anchor: selected.clone(),
                rect,
                tab_strip: None,
                tabs: Vec::new(),
                allow_splits: !floating,
                accepts_tabs: false,
            });
            return;
        }

        let strip = Rect::from_min_size(rect.min, vec2(rect.width(), self.style.tab_height.min(rect.height())));
        ui.painter().rect_filled(strip, 0.0, self.style.tab_background);
        let mut left = strip.left();
        if let Host::Floating(group_rect) = host {
            let grip = Rect::from_min_max(strip.min, pos2((strip.left() + GRIP).min(strip.right()), strip.bottom()));
            self.grip(ui, panels, grip, group_rect);
            left = grip.right();
        }

        let header_w = {
            let w = self.content.group_header_width(panels, selected, floating);
            if w.is_finite() { w.max(0.0).min((strip.right() - left).max(0.0)) } else { 0.0 }
        };
        if header_w > 0.0 {
            let header = Rect::from_min_max(pos2(strip.right() - header_w, strip.top()), strip.max);
            let mut child = ui.new_child(UiBuilder::new().id(self.id.with(("header", selected))).max_rect(header));
            child.set_clip_rect(header.intersect(ui.clip_rect()));
            self.content.group_header(&mut child, panels, selected, floating);
        }

        // Tabs.
        let font = self.style.font.clone();
        let mut shaped = Vec::with_capacity(panels.len());
        for (i, p) in panels.iter().enumerate() {
            let logical = (self.label)(p);
            let color = if i == active { self.style.text } else { self.style.inactive_text };
            let galley = self.content.label(ui, p, &logical, &font, color).into_galley(ui, Some(TextWrapMode::Extend), f32::INFINITY, font.clone());
            shaped.push((logical, color, galley));
        }
        let widths: Vec<f32> = shaped.iter().map(|(_, _, g)| g.size().x + TAB_PADDING).collect();
        let room = (strip.right() - header_w - left).max(0.0);
        let fitted = fit(&widths, active, room, TAB_MIN, OVERFLOW);
        let mut x = left;
        let mut tab_rects: Vec<(P, Rect)> = Vec::with_capacity(panels.len());
        let mut shown = vec![false; panels.len()];
        for &(i, w) in &fitted.shown {
            let (Some(p), Some((logical, color, galley))) = (panels.get(i), shaped.get(i)) else { continue };
            let tab_rect = Rect::from_min_size(pos2(x, strip.top()), vec2(w, strip.height()));
            x += w;
            let group_rect = match host {
                Host::Floating(r) => r,
                Host::Docked => rect,
            };
            self.tab(ui, p, tab_rect, i == active, group_rect, galley.clone(), *color, logical);
            tab_rects.push((p.clone(), tab_rect));
            if let Some(s) = shown.get_mut(i) {
                *s = true;
            }
        }
        if !fitted.overflow.is_empty() && fitted.overflow_width > 0.0 {
            let button = Rect::from_min_size(pos2(x, strip.top()), vec2(fitted.overflow_width, strip.height()));
            let hidden: Vec<P> = fitted.overflow.iter().filter_map(|&i| panels.get(i).cloned()).collect();
            self.overflow(ui, selected, button, &hidden);
        }
        for (i, p) in panels.iter().enumerate() {
            if !shown.get(i).copied().unwrap_or(false) {
                tab_rects.push((p.clone(), Rect::NOTHING));
            }
        }

        self.targets.push(DropTarget {
            anchor: selected.clone(),
            rect,
            tab_strip: Some(strip),
            tabs: tab_rects,
            allow_splits: !floating,
            accepts_tabs: (self.permissions)(selected).accepts_tabs,
        });

        let body = Rect::from_min_max(pos2(rect.left(), strip.bottom()), rect.max);
        if usable(body) {
            self.body(ui, selected, body);
        }
        ui.painter().rect_stroke(rect, 0.0, self.style.border, StrokeKind::Inside);
    }

    fn overflow(&mut self, ui: &mut Ui, selected: &P, button: Rect, hidden: &[P]) {
        let id = overflow_id(self.id, selected);
        let r = ui.interact(button, id, Sense::click());
        let enabled = ui.is_enabled();
        let label = self.style.panels_label.clone();
        r.widget_info(|| WidgetInfo::labeled(WidgetType::Button, enabled, &label));
        if r.hovered() {
            ui.painter().rect_filled(button, 0.0, self.style.active_background);
        }
        ui.painter().with_clip_rect(button.intersect(ui.clip_rect())).text(
            button.center(),
            Align2::CENTER_CENTER,
            "\u{2026}",
            self.style.font.clone(),
            self.style.text,
        );
        egui::Popup::menu(&r).show(|ui| {
            ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                for p in hidden {
                    let logical = (self.label)(p);
                    let color = ui.visuals().text_color();
                    let visual = self.content.label(ui, p, &logical, &self.style.font, color);
                    if menu_item(ui, visual, &logical).clicked() {
                        self.out.actions.push(Action::Activate { panel: p.clone() });
                        self.out.focused = Some(p.clone());
                        ui.close();
                    }
                }
            });
        });
    }

    /// The move grip of a floating group.
    fn grip(&mut self, ui: &mut Ui, panels: &[P], grip: Rect, group_rect: Rect) {
        let first = panels.first();
        let id = grip_id(self.id, first);
        self.own(id);
        let r = ui.interact(grip, id, Sense::drag());
        let enabled = ui.is_enabled();
        let label = self.style.move_label.clone();
        r.widget_info(|| WidgetInfo::labeled(WidgetType::Button, enabled, &label));
        if r.hovered() || r.dragged() {
            ui.ctx().set_cursor_icon(CursorIcon::Grab);
        }
        let c = grip.center();
        ui.painter().line_segment([pos2(c.x, c.y - 5.0), pos2(c.x, c.y + 5.0)], self.style.border);

        let Some(first) = first else { return };
        if !panels.iter().all(|p| self.movable(p)) {
            return;
        }
        let key = group_drag_key(self.id);
        if r.drag_started() {
            let origin = ui.input(|i| i.pointer.press_origin().or(i.pointer.interact_pos())).filter(|p| p.is_finite());
            if let Some(origin) = origin {
                self.ctx.data_mut(|d| d.insert_temp(key, GroupDrag { first: first.clone(), original: group_rect, origin }));
            }
        }
        if r.dragged() && !self.escape && !self.unfocused {
            let record = self.ctx.data(|d| d.get_temp::<GroupDrag<P>>(key));
            let pointer = ui.input(|i| i.pointer.latest_pos()).filter(|p| p.is_finite());
            if let (Some(record), Some(pointer)) = (record, pointer)
                && record.first == *first
            {
                let moved = clamp_to_screen(record.original.translate(pointer - record.origin), self.screen);
                if moved != group_rect {
                    self.out.actions.push(Action::MoveFloating { panel: first.clone(), rect: to_array(moved) });
                }
            }
        }
    }

    /// One tab: `group_rect` is where a drag of it starts from.
    #[allow(clippy::too_many_arguments)]
    fn tab(
        &mut self,
        ui: &mut Ui,
        p: &P,
        rect: Rect,
        active: bool,
        group_rect: Rect,
        galley: std::sync::Arc<egui::Galley>,
        color: Color32,
        logical: &str,
    ) {
        let id = tab_id(self.id, p);
        self.own(id);
        let perms = (self.permissions)(p);
        let sense = if perms.movable { Sense::click_and_drag() } else { Sense::click() };
        let (active_bg, bg) = (self.style.active_background, self.style.tab_background);
        let response = Tab::new(id, logical, active).sense(sense).focus_stroke(Stroke::new(1.0, self.style.accent)).show_at(ui, rect, |ui, r| {
            let fill = if active || r.hovered() { active_bg } else { bg };
            ui.painter().rect_filled(rect, 0.0, fill);
            let pos = rect.center() - galley.size() / 2.0;
            ui.painter().with_clip_rect(rect.intersect(ui.clip_rect())).galley(pos, galley, color);
        });
        if response.clicked() {
            self.out.actions.push(Action::Activate { panel: p.clone() });
            self.out.focused = Some(p.clone());
        }
        if response.drag_started() && perms.movable {
            begin_drag(&self.ctx, self.id, p.clone(), group_rect);
        }
        response.context_menu(|ui| self.tab_menu(ui, p, perms, group_rect));
    }

    fn tab_menu(&mut self, ui: &mut Ui, p: &P, perms: Permissions, group_rect: Rect) {
        self.content.panel_menu(ui, p);
        let font = self.style.font.clone();
        let color = ui.visuals().text_color();
        if perms.movable && perms.floatable {
            let logical = self.style.float_label.clone();
            let visual = self.content.action_label(ui, &logical, &font, color);
            if menu_item(ui, visual, &logical).clicked() {
                let rect = clamp_to_screen(group_rect.translate(Vec2::splat(FLOAT_OFFSET)), self.screen);
                self.out.actions.push(Action::Float { panel: p.clone(), rect: to_array(rect) });
                ui.close();
            }
        }
        if perms.movable {
            let logical = self.style.move_label.clone();
            let visual = self.content.action_label(ui, &logical, &font, color);
            let targets: Vec<P> = self.layout.panels().into_iter().filter(|t| *t != p && (self.permissions)(t).accepts_tabs).cloned().collect();
            let (response, _) = egui::containers::menu::SubMenuButton::from_button(Button::new(visual).right_text("\u{23f5}")).ui(ui, |ui| {
                for t in &targets {
                    let name = (self.label)(t);
                    let visual = self.content.label(ui, t, &name, &font, color);
                    if menu_item(ui, visual, &name).clicked() {
                        self.out.actions.push(Action::Move { panel: p.clone(), anchor: t.clone(), placement: Placement::Tab { before: None } });
                        ui.close();
                    }
                }
            });
            let enabled = ui.is_enabled();
            response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, enabled, &logical));
        }
        if perms.closable {
            let logical = self.style.close_label.clone();
            let visual = self.content.action_label(ui, &logical, &font, color);
            if menu_item(ui, visual, &logical).clicked() {
                self.out.actions.push(Action::Close { panel: p.clone() });
                ui.close();
            }
        }
    }

    fn body(&mut self, ui: &mut Ui, p: &P, rect: Rect) {
        ui.painter().rect_filled(rect, 0.0, self.style.background);
        let mut child = ui.new_child(UiBuilder::new().id(self.id.with(("body", p))).max_rect(rect).layout(egui::Layout::top_down(Align::Min)));
        child.set_clip_rect(rect.intersect(ui.clip_rect()));
        self.content.body(&mut child, p);
        let layer = ui.layer_id();
        let pressed = ui.input(|i| i.pointer.primary_pressed().then(|| i.pointer.interact_pos()).flatten());
        if let Some(at) = pressed
            && rect.contains(at)
            && self.ctx.layer_id_at(at).is_none_or(|l| l == layer)
        {
            self.out.focused = Some(p.clone());
        }
    }

    // -----------------------------------------------------------------------------------------
    // Floating groups

    fn floating_groups(&mut self) {
        let layout = self.layout;
        let order: Vec<LayerId> = self.ctx.memory(|m| m.layer_ids().collect());
        let mut groups: Vec<(Option<usize>, usize)> = layout
            .floating
            .iter()
            .enumerate()
            .map(|(i, g)| {
                let at = g.panels.first().and_then(|f| {
                    let layer = LayerId::new(Order::Middle, floating_id(self.id, f));
                    order.iter().position(|l| *l == layer)
                });
                (at, i)
            })
            .collect();
        // Known layers in their current order, unknown ones last (by index).
        groups.sort_by_key(|&(at, i)| (at.is_none(), at.unwrap_or(0), i));

        for (_, i) in groups {
            let Some(group) = layout.floating.get(i) else { continue };
            let Some(first) = group.panels.first() else { continue };
            let fid = floating_id(self.id, first);
            let limits = self.group_limits(&group.panels);
            let stored = from_array(group.rect);
            let size = stored.size().max(limits.min).min(limits.max);
            let rect = clamp_to_screen(Rect::from_min_size(stored.min, size), self.screen);

            if let Some(raise) = &self.raise
                && group.panels.contains(raise)
            {
                let raise = raise.clone();
                self.ctx.move_to_top(LayerId::new(Order::Middle, fid));
                self.ctx.memory_mut(|m| m.request_focus(tab_id(self.id, &raise)));
                self.out.focused = Some(raise);
            }

            let ctx = self.ctx.clone();
            egui::Area::new(fid).order(Order::Middle).fixed_pos(rect.min).movable(false).constrain_to(self.screen).show(&ctx, |ui| {
                self.group(ui, &group.panels, group.active, rect, Host::Floating(rect));
                self.resize_corner(ui, first, fid, rect, limits);
                ui.advance_cursor_after_rect(rect);
            });
        }
    }

    fn resize_corner(&mut self, ui: &mut Ui, first: &P, fid: Id, rect: Rect, limits: PanelLimits) {
        let corner = Rect::from_min_size(rect.max - Vec2::splat(CORNER), Vec2::splat(CORNER));
        let id = fid.with("resize");
        self.own(id);
        let r = ui.interact(corner, id, Sense::drag());
        let enabled = ui.is_enabled();
        let label = self.style.resize_window_label.clone();
        r.widget_info(|| WidgetInfo::labeled(WidgetType::Button, enabled, &label));
        if r.drag_started() {
            r.request_focus();
        }
        let mut delta = if r.dragged() { r.drag_delta() } else { Vec2::ZERO };
        if r.has_focus() {
            ui.memory_mut(|m| m.set_focus_lock_filter(id, EventFilter { horizontal_arrows: true, vertical_arrows: true, ..Default::default() }));
            delta += ui.input_mut(|i| {
                let mut d = Vec2::ZERO;
                for (key, step) in [
                    (Key::ArrowLeft, vec2(-KEY_STEP, 0.0)),
                    (Key::ArrowRight, vec2(KEY_STEP, 0.0)),
                    (Key::ArrowUp, vec2(0.0, -KEY_STEP)),
                    (Key::ArrowDown, vec2(0.0, KEY_STEP)),
                ] {
                    while i.consume_key(Modifiers::NONE, key) {
                        d += step;
                    }
                }
                d
            });
        }
        if r.hovered() || r.dragged() {
            ui.ctx().set_cursor_icon(CursorIcon::ResizeNwSe);
        }
        if r.hovered() || r.dragged() || r.has_focus() {
            ui.painter().line_segment([corner.left_bottom(), corner.right_top()], Stroke::new(2.0, self.style.accent));
        }
        if delta != Vec2::ZERO && delta.is_finite() {
            let floor = limits.min.max(vec2(TAB_MIN, self.style.tab_height + 20.0));
            let size = (rect.size() + delta).max(floor).min(limits.max).min(self.screen.size());
            self.out.actions.push(Action::MoveFloating { panel: first.clone(), rect: [rect.min.x, rect.min.y, size.x, size.y] });
        }
    }

    // -----------------------------------------------------------------------------------------
    // Drag completion

    fn finish_group_drag(&mut self) {
        let key = group_drag_key(self.id);
        let Some(record) = self.ctx.data(|d| d.get_temp::<GroupDrag<P>>(key)) else { return };
        let group = self.layout.floating.iter().find(|g| g.panels.first() == Some(&record.first));
        if group.is_none_or(|g| !g.panels.iter().all(|p| self.movable(p))) {
            self.ctx.data_mut(|d| d.remove::<GroupDrag<P>>(key));
            return;
        }
        let (down, pointer) = self.ctx.input(|i| (i.pointer.primary_down(), i.pointer.latest_pos()));
        let pointer = pointer.filter(|p| p.is_finite());
        if self.escape || self.unfocused || !down {
            self.ctx.data_mut(|d| d.remove::<GroupDrag<P>>(key));
            if self.escape || self.unfocused {
                self.out.actions.push(Action::MoveFloating { panel: record.first.clone(), rect: to_array(record.original) });
            } else if let Some(at) = pointer
                && !self.blocked(at)
            {
                self.out.group_drop = Some((record.first, at));
            }
            return;
        }
        if pointer.is_some_and(|p| !self.blocked(p)) {
            self.out.group_drag = Some(record.first);
        }
    }

    fn finish_tab_drag(&mut self) {
        let key = drag_key(self.id);
        let Some(record) = self.ctx.data(|d| d.get_temp::<TabDrag<P>>(key)) else { return };
        let escape = self.ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) || self.escape;
        let (down, released, pointer) = self.ctx.input(|i| (i.pointer.primary_down(), i.pointer.primary_released(), i.pointer.latest_pos()));
        if escape || self.unfocused || !self.layout.contains(&record.panel) || !(down || released) {
            cancel_drag::<P>(&self.ctx, self.id);
            return;
        }
        let Some(pointer) = pointer.filter(|p| p.is_finite()) else {
            if released {
                cancel_drag::<P>(&self.ctx, self.id);
            }
            return;
        };
        if self.blocked(pointer) {
            if released {
                cancel_drag::<P>(&self.ctx, self.id);
            }
            return;
        }
        let proposal = resolve_drop(&record.panel, pointer, &self.targets);
        let perms = (self.permissions)(&record.panel);
        let already_floating = self.layout.floating.iter().any(|g| g.panels.contains(&record.panel));
        let float_rect = (perms.floatable && (!record.source.contains(pointer) || already_floating))
            .then(|| clamp_to_screen(Rect::from_min_size(pointer - record.grab, record.source.size()), self.screen));

        if released {
            self.ctx.data_mut(|d| d.remove::<TabDrag<P>>(key));
            if pointer.distance(record.origin) <= CLICK_SLOP {
                return;
            }
            let action = match (&proposal, float_rect) {
                (Some(DropProposal { anchor, placement, .. }), _) => {
                    Some(Action::Move { panel: record.panel.clone(), anchor: anchor.clone(), placement: placement.clone() })
                }
                (None, Some(rect)) => Some(Action::Float { panel: record.panel.clone(), rect: to_array(rect) }),
                (None, None) => None,
            };
            if let Some(action) = action {
                let mut probe = self.layout.clone();
                if probe.apply_with_permissions(action.clone(), self.permissions).is_ok() {
                    self.out.actions.push(action);
                    self.out.focused = Some(record.panel);
                }
            }
            return;
        }

        let painter = self.ctx.layer_painter(LayerId::new(Order::Foreground, self.id.with("drop-preview")));
        let accent = self.style.accent;
        match (&proposal, float_rect) {
            (Some(proposal), _) => {
                painter.rect(proposal.preview.shrink(1.0), 0.0, accent.gamma_multiply(0.15), Stroke::new(2.0, accent), StrokeKind::Inside);
                if let Some(marker) = proposal.insertion {
                    painter.rect_filled(marker, 0.0, accent);
                }
            }
            (None, Some(rect)) => {
                painter.rect_stroke(rect, 0.0, Stroke::new(2.0, accent), StrokeKind::Inside);
            }
            (None, None) => {}
        }
    }
}

/// A menu button whose visible text may differ from its accessible name.
fn menu_item(ui: &mut Ui, visual: WidgetText, logical: &str) -> Response {
    let response = ui.add(Button::new(visual));
    let enabled = ui.is_enabled();
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, enabled, logical));
    response
}
