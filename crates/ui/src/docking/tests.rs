use egui::{Event, Id, Key, Modifiers, PointerButton, Pos2, Rect, Ui, Vec2, pos2, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};

use super::*;
use crate::layout::{SplitAxis, SplitSize};

type L = Layout<u32>;

fn tabs(panels: &[u32], active: usize) -> Node<u32> {
    Node::Tabs { panels: panels.to_vec(), active }
}

fn split(axis: SplitAxis, size: SplitSize, first: Node<u32>, second: Node<u32>) -> Node<u32> {
    Node::Split { axis, size, first: Box::new(first), second: Box::new(second) }
}

fn entry(panel: u32, open: bool, height: Option<f32>) -> StackEntry<u32> {
    StackEntry { panel, open, height }
}

fn sample() -> L {
    Layout {
        root: Some(split(SplitAxis::Horizontal, SplitSize::FixedSecond(240.0), tabs(&[1], 0), tabs(&[2, 3, 4], 1))),
        floating: vec![Floating { panels: vec![5, 6], active: 1, rect: [80.0, 70.0, 300.0, 220.0] }],
    }
}

fn tab_before(before: Option<u32>) -> Placement<u32> {
    Placement::Tab { before }
}

fn sorted(layout: &L) -> Vec<u32> {
    let mut all: Vec<u32> = layout.panels().into_iter().copied().collect();
    all.sort_unstable();
    all
}

fn second_of(layout: &L) -> &Node<u32> {
    match &layout.root {
        Some(Node::Split { second, .. }) => second,
        other => panic!("not a split: {other:?}"),
    }
}

// ---------------------------------------------------------------------------------------------
// Model

#[test]
fn moving_a_floating_tab_into_a_docked_group() {
    let mut l = sample();
    l.apply(Action::Move { panel: 5, anchor: 3, placement: tab_before(Some(4)) }).unwrap();
    assert_eq!(second_of(&l), &tabs(&[2, 3, 5, 4], 2));
    assert!(matches!(l.root, Some(Node::Split { size: SplitSize::FixedSecond(240.0), .. })));
    assert_eq!(l.floating, vec![Floating { panels: vec![6], active: 0, rect: [80.0, 70.0, 300.0, 220.0] }]);
    assert_eq!(l.panels().len(), 6);
    l.validate().unwrap();
}

#[test]
fn floating_splitting_and_merging_back() {
    let mut l = sample();
    l.apply(Action::Float { panel: 1, rect: [10.0, 10.0, 200.0, 150.0] }).unwrap();
    assert_eq!(l.root, Some(tabs(&[2, 3, 4], 1)));
    l.apply(Action::Move { panel: 1, anchor: 2, placement: Placement::Split(Zone::Left) }).unwrap();
    assert_eq!(l.root, Some(split(SplitAxis::Horizontal, SplitSize::Ratio(0.5), tabs(&[1], 0), tabs(&[2, 3, 4], 1))));
    assert_eq!(l.floating.len(), 1);
    l.apply(Action::Move { panel: 5, anchor: 6, placement: Placement::Split(Zone::Center) }).unwrap();
    assert_eq!(l.floating[0].panels, vec![6, 5]);
    assert_eq!(l.floating[0].active, 1);
    l.apply(Action::Move { panel: 6, anchor: 3, placement: tab_before(None) }).unwrap();
    l.apply(Action::Move { panel: 5, anchor: 3, placement: tab_before(None) }).unwrap();
    assert!(l.floating.is_empty());
    assert_eq!(sorted(&l), vec![1, 2, 3, 4, 5, 6]);
}

#[test]
fn rejected_transactions_change_nothing() {
    let cases = [
        (Action::Move { panel: 1, anchor: 99, placement: tab_before(None) }, DockError::MissingTarget),
        (Action::Move { panel: 99, anchor: 1, placement: tab_before(None) }, DockError::MissingPanel),
        (Action::Move { panel: 1, anchor: 5, placement: Placement::Split(Zone::Left) }, DockError::InvalidPlacement),
        (Action::Move { panel: 1, anchor: 1, placement: tab_before(None) }, DockError::InvalidPlacement),
        (Action::Move { panel: 1, anchor: 3, placement: tab_before(Some(6)) }, DockError::MissingTarget),
        (Action::Float { panel: 1, rect: [f32::NAN, 0.0, 10.0, 10.0] }, DockError::InvalidGeometry),
        (Action::Float { panel: 1, rect: [0.0, 0.0, 0.0, 10.0] }, DockError::InvalidGeometry),
        (Action::ResizeSplit { path: vec![false], size: SplitSize::Ratio(0.5) }, DockError::MissingTarget),
    ];
    for (action, error) in cases {
        let mut l = sample();
        assert_eq!(l.apply(action.clone()), Err(error), "{action:?}");
        assert_eq!(l, sample(), "{action:?}");
    }
}

#[test]
fn closing_keeps_the_selected_identity() {
    let mut l = Layout { root: Some(tabs(&[1, 2, 3, 4], 2)), floating: vec![] };
    l.apply(Action::Close { panel: 1 }).unwrap();
    assert_eq!(l.root, Some(tabs(&[2, 3, 4], 1)));
    l.apply(Action::Close { panel: 3 }).unwrap();
    assert_eq!(l.root, Some(tabs(&[2, 4], 1)));
}

#[test]
fn reordering_inside_a_group() {
    let mut l = Layout { root: Some(tabs(&[1, 2, 3], 0)), floating: vec![] };
    l.apply(Action::Move { panel: 1, anchor: 2, placement: tab_before(None) }).unwrap();
    assert_eq!(l.root, Some(tabs(&[2, 3, 1], 2)));
    l.apply(Action::Move { panel: 1, anchor: 2, placement: tab_before(Some(2)) }).unwrap();
    assert_eq!(l.root, Some(tabs(&[1, 2, 3], 0)));
    let before = l.clone();
    l.apply(Action::Move { panel: 1, anchor: 2, placement: tab_before(Some(1)) }).unwrap();
    assert_eq!(l, before);
}

#[test]
fn accordion_entries_keep_their_state() {
    let mut l = Layout { root: Some(Node::Stack { entries: vec![entry(1, false, Some(123.0)), entry(2, true, None)] }), floating: vec![] };
    l.apply(Action::Move { panel: 1, anchor: 2, placement: Placement::Split(Zone::Center) }).unwrap();
    assert_eq!(l.root, Some(Node::Stack { entries: vec![entry(2, true, None), entry(1, false, Some(123.0))] }));
    let original = l.clone();
    let loc = l.location(&1).unwrap();
    assert_eq!(loc.stack, Some((false, Some(123.0))));
    l.apply(Action::Close { panel: 1 }).unwrap();
    l.restore(1, &loc).unwrap();
    assert_eq!(l, original);
}

#[test]
fn protected_panels_refuse_changes() {
    let base = Layout { root: Some(split(SplitAxis::Horizontal, SplitSize::Ratio(0.5), tabs(&[1], 0), tabs(&[2, 3], 0))), floating: vec![] };
    let perms = |p: &u32| if *p == 1 { Permissions::PROTECTED } else { Permissions::default() };
    let refused = [
        Action::Close { panel: 1 },
        Action::Float { panel: 1, rect: [0.0, 0.0, 100.0, 100.0] },
        Action::Move { panel: 1, anchor: 3, placement: Placement::Split(Zone::Center) },
        Action::Move { panel: 3, anchor: 1, placement: tab_before(None) },
        Action::OpenAt { panel: 7, anchor: 1, placement: tab_before(None) },
    ];
    for action in refused {
        let mut l = base.clone();
        assert_eq!(l.apply_with_permissions(action.clone(), perms), Err(DockError::NotAllowed), "{action:?}");
        assert_eq!(l, base);
    }
    let mut l = base.clone();
    l.apply_with_permissions(Action::OpenAt { panel: 7, anchor: 1, placement: Placement::Split(Zone::Left) }, perms).unwrap();
    assert!(l.contains(&7));
}

#[test]
fn locations_restore_splits_and_floating_groups() {
    let original = Layout {
        root: Some(split(SplitAxis::Horizontal, SplitSize::FixedFirst(180.0), tabs(&[1], 0), tabs(&[2], 0))),
        floating: vec![Floating { panels: vec![3], active: 0, rect: [5.0, 6.0, 120.0, 90.0] }],
    };
    let mut l = original.clone();
    let loc1 = l.location(&1).unwrap();
    assert_eq!(loc1.placement, Placement::Split(Zone::Left));
    assert_eq!((loc1.anchor, loc1.size, loc1.sibling.clone()), (Some(2), Some(SplitSize::FixedFirst(180.0)), vec![2]));
    l.apply(Action::Close { panel: 1 }).unwrap();
    let loc2 = l.location(&2).unwrap();
    l.apply(Action::Close { panel: 2 }).unwrap();
    let loc3 = l.location(&3).unwrap();
    assert_eq!(loc3.floating, Some([5.0, 6.0, 120.0, 90.0]));
    l.apply(Action::Close { panel: 3 }).unwrap();
    assert_eq!(l, Layout::default());
    l.restore(3, &loc3).unwrap();
    l.restore(2, &loc2).unwrap();
    l.restore(1, &loc1).unwrap();
    assert_eq!(l, original);

    let mut missing = loc1.clone();
    missing.anchor = Some(42);
    let mut l2 = Layout { root: Some(tabs(&[2], 0)), floating: vec![] };
    let before = l2.clone();
    assert_eq!(l2.restore(1, &missing), Err(DockError::MissingTarget));
    assert_eq!(l2, before);
    assert_eq!(l2.restore(2, &loc2), Err(DockError::DuplicatePanel));
}

#[test]
fn closed_accordion_entries_come_back_in_reverse_order() {
    let original = Layout {
        root: Some(split(
            SplitAxis::Horizontal,
            SplitSize::FixedSecond(220.0),
            tabs(&[1], 0),
            Node::Stack { entries: vec![entry(2, false, Some(90.0)), entry(3, true, Some(180.0))] },
        )),
        floating: vec![],
    };
    let mut l = original.clone();
    let loc2 = l.location(&2).unwrap();
    l.apply(Action::Close { panel: 2 }).unwrap();
    let loc3 = l.location(&3).unwrap();
    l.apply(Action::Close { panel: 3 }).unwrap();
    assert_eq!(l.root, Some(tabs(&[1], 0)));
    l.restore(3, &loc3).unwrap();
    l.restore(2, &loc2).unwrap();
    assert_eq!(l, original);
}

#[test]
fn a_panel_beside_a_nested_split_comes_back() {
    let original = Layout {
        root: Some(split(
            SplitAxis::Vertical,
            SplitSize::FixedSecond(330.0),
            tabs(&[1], 0),
            split(SplitAxis::Horizontal, SplitSize::FixedFirst(220.0), tabs(&[2], 0), tabs(&[3], 0)),
        )),
        floating: vec![],
    };
    let mut l = original.clone();
    let loc = l.location(&1).unwrap();
    assert_eq!(loc.placement, Placement::Split(Zone::Top));
    assert_eq!(loc.sibling, vec![2, 3]);
    l.apply(Action::Close { panel: 1 }).unwrap();
    l.restore(1, &loc).unwrap();
    assert_eq!(l, original);
}

#[test]
fn validation_rejects_bad_layouts() {
    let dup = Layout { root: Some(tabs(&[1, 2], 0)), floating: vec![Floating { panels: vec![2], active: 0, rect: [0.0, 0.0, 10.0, 10.0] }] };
    assert_eq!(dup.validate(), Err(DockError::DuplicatePanel));
    let selection = Layout { root: Some(tabs(&[1], usize::MAX)), floating: vec![] };
    assert_eq!(selection.validate(), Err(DockError::InvalidSelection));
    let geometry = Layout { root: None, floating: vec![Floating { panels: vec![1], active: 0, rect: [0.0, 0.0, 10.0, f32::INFINITY] }] };
    assert_eq!(geometry.validate(), Err(DockError::InvalidGeometry));
    let empty_placeholder: L = Layout { root: Some(tabs(&[], 0)), floating: vec![] };
    empty_placeholder.validate().unwrap();

    let mut deep = tabs(&[0], 0);
    for i in 1..65 {
        deep = split(SplitAxis::Horizontal, SplitSize::Ratio(0.5), deep, tabs(&[i], 0));
    }
    let mut deep = Layout { root: Some(deep), floating: vec![] };
    assert_eq!(deep.validate(), Err(DockError::LimitExceeded));
    assert_eq!(deep.apply(Action::Close { panel: 3 }), Err(DockError::LimitExceeded));

    let many: Vec<u32> = (0..4097).collect();
    let wide = Layout { root: Some(tabs(&many, 0)), floating: vec![] };
    assert_eq!(wide.validate(), Err(DockError::LimitExceeded));
}

#[test]
fn drop_resolution() {
    let rect = Rect::from_min_size(pos2(10.0, 20.0), vec2(300.0, 200.0));
    let strip = Rect::from_min_size(rect.min, vec2(300.0, 24.0));
    let t2 = Rect::from_min_size(rect.min, vec2(60.0, 24.0));
    let t3 = Rect::from_min_size(pos2(70.0, 20.0), vec2(60.0, 24.0));
    let target = DropTarget { anchor: 2u32, rect, tab_strip: Some(strip), tabs: vec![(2, t2), (3, t3)], allow_splits: true, accepts_tabs: true };
    let targets = vec![target.clone()];

    let p = resolve_drop(&7, pos2(90.0, 30.0), &targets).unwrap();
    assert_eq!(p.placement, Placement::Tab { before: Some(3) });
    assert!(p.insertion.is_some_and(|r| r.is_finite() && strip.contains_rect(r)));

    assert_eq!(resolve_drop(&7, rect.center(), &targets).unwrap().placement, Placement::Split(Zone::Center));
    let left = resolve_drop(&7, pos2(15.0, 120.0), &targets).unwrap();
    assert_eq!(left.placement, Placement::Split(Zone::Left));
    assert_eq!(left.preview, zone_rect(rect, Zone::Left).unwrap());
    assert_eq!(resolve_drop(&3, rect.center(), &targets), None);
    assert!(resolve_drop(&3, pos2(15.0, 120.0), &targets).is_some());

    let overlay = DropTarget {
        anchor: 9u32,
        rect: Rect::from_center_size(rect.center(), vec2(80.0, 80.0)),
        tab_strip: None,
        tabs: vec![(9, Rect::NOTHING)],
        allow_splits: false,
        accepts_tabs: false,
    };
    assert_eq!(resolve_drop(&7, rect.center(), &[target, overlay]), None);

    assert_eq!(drop_zone(Rect::NOTHING, 0.0, pos2(1.0, 1.0)), None);
    assert_eq!(drop_zone(rect, 0.0, pos2(f32::NAN, 30.0)), None);
    assert_eq!(drop_zone(rect, 24.0, pos2(12.0, 25.0)), Some(Zone::Center));
}

#[test]
fn random_moves_keep_the_layout_valid() {
    let mut l = Layout {
        root: Some(split(SplitAxis::Horizontal, SplitSize::FixedSecond(240.0), tabs(&[1], 0), tabs(&[2, 3, 4], 1))),
        floating: vec![Floating { panels: vec![5, 6], active: 1, rect: [80.0, 70.0, 300.0, 220.0] }],
    };
    let zones = [Zone::Center, Zone::Left, Zone::Right, Zone::Top, Zone::Bottom];
    let mut seed: u64 = 72;
    for step in 0..800 {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let panel = (seed % 6) as u32 + 1;
        let anchor = ((seed >> 16) % 6) as u32 + 1;
        let action = if step % 7 == 0 {
            Action::Float { panel, rect: [20.0, 20.0, 160.0, 120.0] }
        } else {
            Action::Move { panel, anchor, placement: Placement::Split(zones[(seed % 5) as usize]) }
        };
        let before = l.clone();
        if l.apply(action).is_err() {
            assert_eq!(l, before);
        }
        l.validate().unwrap();
        assert_eq!(sorted(&l), vec![1, 2, 3, 4, 5, 6]);
    }
}

#[cfg(feature = "serde")]
#[test]
fn layouts_and_locations_round_trip() {
    let l = sample();
    let json = serde_json::to_string(&l).unwrap();
    let back: L = serde_json::from_str(&json).unwrap();
    assert_eq!(back, l);
    back.validate().unwrap();
    let loc = l.location(&3).unwrap();
    let back: Location<u32> = serde_json::from_str(&serde_json::to_string(&loc).unwrap()).unwrap();
    assert_eq!(back, loc);

    #[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
    enum Kind {
        Canvas,
        Layers,
    }
    let legacy = r#"{"floating":null,"anchor":"Layers","placement":{"Split":"Left"},"size":{"Ratio":0.5},"stack":null}"#;
    let loc: Location<Kind> = serde_json::from_str(legacy).unwrap();
    assert!(loc.sibling.is_empty());
    assert_eq!(loc.anchor, Some(Kind::Layers));
    let _ = Kind::Canvas;
}

// ---------------------------------------------------------------------------------------------
// Renderer

const AREA: &str = "dock";

fn area() -> Id {
    Id::new(AREA)
}

struct Dock {
    layout: L,
    actions: Vec<Action<u32>>,
    output: DockOutput<u32>,
    permissions: fn(&u32) -> Permissions,
    limits: fn(&u32) -> PanelLimits,
    blockers: Vec<Rect>,
    body_ids: std::collections::HashMap<u32, Id>,
    body_rects: std::collections::HashMap<u32, Rect>,
    apply: bool,
    raise: Option<u32>,
    group_drags: Vec<u32>,
    group_drops: Vec<(u32, Pos2)>,
}

impl Dock {
    fn new(layout: L) -> Self {
        Self {
            layout,
            actions: Vec::new(),
            output: DockOutput::default(),
            permissions: |_| Permissions::default(),
            limits: |_| PanelLimits::default(),
            blockers: Vec::new(),
            body_ids: Default::default(),
            body_rects: Default::default(),
            apply: true,
            raise: None,
            group_drags: Vec::new(),
            group_drops: Vec::new(),
        }
    }

    fn show(&mut self, ui: &mut Ui) {
        let style = DockStyle::from_ui(ui);
        let mut dock = DockArea::new(area()).blocked_rects(&self.blockers);
        if let Some(p) = self.raise.take() {
            dock = dock.raise_panel(p);
        }
        let (ids, rects) = (&mut self.body_ids, &mut self.body_rects);
        let out = dock.show_with_limits(
            ui,
            &self.layout,
            &style,
            |p| format!("Panel {p}"),
            self.permissions,
            self.limits,
            |ui, p| {
                ids.insert(*p, ui.id());
                rects.insert(*p, ui.max_rect());
                ui.label(format!("Body {p}"));
            },
        );
        if self.apply {
            for a in &out.actions {
                self.layout.apply_with_permissions(a.clone(), self.permissions).unwrap();
            }
        }
        self.actions.extend(out.actions.iter().cloned());
        self.group_drags.extend(out.group_drag);
        self.group_drops.extend(out.group_drop);
        self.output = out;
    }
}

fn harness(dock: Dock, size: Vec2) -> Harness<'static, Dock> {
    let mut h = Harness::builder().with_size(size).build_ui_state(|ui, d: &mut Dock| d.show(ui), dock);
    h.step();
    h
}

fn rect_of(h: &Harness<'_, Dock>, id: Id) -> Rect {
    h.ctx.read_response(id).map(|r| r.rect).unwrap_or_else(|| panic!("{id:?} not rendered"))
}

fn tab(p: u32) -> Id {
    area().with(("tab", p))
}

fn drag(h: &mut Harness<'_, Dock>, from: Pos2, path: &[Pos2]) {
    h.hover_at(from);
    h.step();
    h.drag_at(from);
    h.step();
    for &p in path {
        h.hover_at(p);
        h.step();
    }
    h.drop_at(*path.last().unwrap_or(&from));
    h.step();
    h.step();
}

#[test]
fn bodies_keep_their_id_when_floated() {
    let mut h = harness(Dock::new(sample()), vec2(900.0, 600.0));
    let before = h.state().body_ids[&3];
    h.state_mut().layout.apply(Action::Activate { panel: 3 }).unwrap();
    h.step();
    let docked = h.state().body_ids[&3];
    h.state_mut().layout.apply(Action::Float { panel: 3, rect: [200.0, 200.0, 200.0, 160.0] }).unwrap();
    h.step();
    h.step();
    assert_eq!(h.state().body_ids[&3], docked);
    assert_ne!(before, Id::NULL);
}

#[test]
fn escape_focus_loss_and_cancelled_releases_do_nothing() {
    let mut h = harness(Dock::new(sample()), vec2(900.0, 600.0));
    let start = rect_of(&h, tab(3)).center();
    // Escape mid drag.
    h.hover_at(start);
    h.step();
    h.drag_at(start);
    h.step();
    h.hover_at(pos2(150.0, 260.0));
    h.step();
    h.key_press(Key::Escape);
    h.step();
    h.drop_at(pos2(150.0, 260.0));
    h.step();
    assert!(h.state().actions.is_empty(), "{:?}", h.state().actions);
    // Window focus lost mid drag.
    h.hover_at(start);
    h.step();
    h.drag_at(start);
    h.step();
    h.hover_at(pos2(150.0, 260.0));
    h.step();
    h.event(Event::WindowFocused(false));
    h.step();
    h.event(Event::WindowFocused(true));
    h.drop_at(pos2(160.0, 300.0));
    h.step();
    h.step();
    assert!(h.state().actions.is_empty(), "{:?}", h.state().actions);
    assert_eq!(h.state().layout, sample());

    // A release after `cancel_drag` does nothing either.
    h.hover_at(start);
    h.step();
    h.drag_at(start);
    h.step();
    h.hover_at(pos2(150.0, 260.0));
    h.step();
    cancel_drag::<u32>(&h.ctx, area());
    h.drop_at(pos2(160.0, 300.0));
    h.step();
    h.step();
    assert!(h.state().actions.is_empty(), "{:?}", h.state().actions);

    // Control: the uninterrupted gesture does produce an action.
    drag(&mut h, start, &[pos2(150.0, 260.0), pos2(160.0, 300.0)]);
    assert!(!h.state().actions.is_empty());
}

#[test]
fn dragging_a_floating_tab_onto_a_docked_tab() {
    let mut h = harness(Dock::new(sample()), vec2(900.0, 600.0));
    let from = rect_of(&h, tab(5)).center();
    let to = rect_of(&h, tab(3)).center();
    drag(&mut h, from, &[from + vec2(8.0, 0.0), to]);
    let s = h.state();
    assert!(s.actions.iter().any(|a| matches!(a, Action::Move { panel: 5, .. })), "{:?}", s.actions);
    assert!(s.layout.floating.iter().all(|g| !g.panels.contains(&5)));
    assert_eq!(s.layout.panels().len(), 6);
    match second_of(&s.layout) {
        Node::Tabs { panels, active } => assert_eq!(panels.get(*active), Some(&5)),
        other => panic!("{other:?}"),
    }
}

fn limited(p: &u32) -> PanelLimits {
    match p {
        1 => PanelLimits { min: vec2(200.0, 0.0), max: vec2(480.0, 100_000.0) },
        2 => PanelLimits { min: vec2(250.0, 0.0), max: vec2(520.0, 100_000.0) },
        0 => PanelLimits { min: vec2(360.0, 0.0), max: Vec2::splat(1_000_000.0) },
        _ => PanelLimits::default(),
    }
}

#[test]
fn limits_shape_the_view_without_rewriting_sizes() {
    let layout = Layout {
        root: Some(split(
            SplitAxis::Horizontal,
            SplitSize::FixedFirst(300.0),
            tabs(&[1], 0),
            split(SplitAxis::Horizontal, SplitSize::FixedSecond(300.0), tabs(&[0], 0), tabs(&[2], 0)),
        )),
        floating: vec![],
    };
    let mut dock = Dock::new(layout.clone());
    dock.permissions = |p| if *p == 0 { Permissions::PROTECTED } else { Permissions::default() };
    dock.limits = limited;
    let mut h = harness(dock, vec2(1400.0, 600.0));
    let start = h.state().body_rects[&1].width();
    for width in [1400.0, 900.0, 300.0, 1400.0] {
        h.set_size(vec2(width, 600.0));
        h.step();
        h.step();
        let s = h.state();
        assert!(s.actions.is_empty(), "{:?}", s.actions);
        for (p, r) in &s.body_rects {
            assert!(r.is_finite() && r.width() > 0.0 && r.height() > 0.0, "{p} {r:?}");
            assert!(r.max.x <= width + 0.5, "{p} {r:?}");
            if width >= 900.0 {
                let l = limited(p);
                assert!(r.width() + 0.5 >= l.min.x && r.width() <= l.max.x + 0.5, "{width} {p} {r:?}");
            }
        }
    }
    assert_eq!(h.state().body_rects[&1].width(), start);
    assert_eq!(h.state().layout, layout);
}

#[test]
fn floating_resize_corner_follows_arrow_keys() {
    let mut h = harness(Dock::new(sample()), vec2(900.0, 600.0));
    let corner = area().with(("floating", 5u32)).with("resize");
    h.ctx.memory_mut(|m| m.request_focus(corner));
    h.step();
    h.key_press(Key::ArrowRight);
    h.step();
    h.key_press(Key::ArrowDown);
    h.step();
    assert_eq!(h.state().layout.floating[0].rect, [80.0, 70.0, 308.0, 228.0]);
    h.key_press(Key::Tab);
    h.step();
    assert_ne!(h.ctx.memory(|m| m.focused()), Some(corner));
}

#[test]
fn accordion_dividers_trade_height_between_open_entries() {
    let layout = Layout {
        root: Some(Node::Stack {
            entries: vec![entry(1, true, Some(150.0)), entry(2, false, Some(91.0)), entry(3, true, Some(150.0)), entry(4, true, Some(120.0))],
        }),
        floating: vec![],
    };
    let mut h = harness(Dock::new(layout), vec2(400.0, 700.0));
    let id = area().with(("stack-resize", 1u32));
    let c = rect_of(&h, id).center();
    drag(&mut h, c, &[c + vec2(0.0, 12.0), c + vec2(0.0, 24.0)]);
    let heights = |h: &Harness<'_, Dock>| match &h.state().layout.root {
        Some(Node::Stack { entries }) => entries.iter().map(|e| (e.open, e.height)).collect::<Vec<_>>(),
        _ => vec![],
    };
    let after = heights(&h);
    let (h1, h3) = (after[0].1.unwrap(), after[2].1.unwrap());
    assert!(h1 > 150.0, "{after:?}");
    assert!((h1 + h3 - 300.0).abs() < 0.01, "{after:?}");
    assert_eq!(after[1], (false, Some(91.0)));
    assert_eq!(after[3], (true, Some(120.0)));
    h.ctx.memory_mut(|m| m.request_focus(id));
    h.step();
    h.key_press(Key::ArrowDown);
    h.step();
    let again = heights(&h);
    let (g1, g3) = (again[0].1.unwrap(), again[2].1.unwrap());
    assert!(g1 > h1 && (g1 + g3 - 300.0).abs() < 0.01, "{again:?}");
}

/// A body with an always-visible scrollbar at its right edge.
fn scrolling_dock(layout: L) -> Harness<'static, Dock> {
    let mut h = Harness::builder().with_size(vec2(800.0, 400.0)).build_ui_state(
        |ui, d: &mut Dock| {
            let style = DockStyle::from_ui(ui);
            let out = DockArea::new(area()).show(
                ui,
                &d.layout,
                &style,
                |p| format!("Panel {p}"),
                |_| Permissions::default(),
                |ui, p| {
                    egui::ScrollArea::vertical().id_salt(p).scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible).show(
                        ui,
                        |ui| {
                            for i in 0..80 {
                                ui.label(format!("line {i}"));
                            }
                        },
                    );
                },
            );
            for a in &out.actions {
                d.layout.apply(a.clone()).unwrap();
            }
            d.actions.extend(out.actions);
        },
        Dock::new(layout),
    );
    h.step();
    h
}

#[test]
fn split_divider_wins_over_a_scrollbar() {
    let layout = Layout { root: Some(split(SplitAxis::Horizontal, SplitSize::FixedFirst(220.0), tabs(&[1], 0), tabs(&[2], 0))), floating: vec![] };
    let mut h = scrolling_dock(layout);
    // The reported rect includes the 3-point grab margin; the left pane ends where the visible
    // divider starts.
    let hit = rect_of(&h, area().with(("split", Vec::<bool>::new())));
    let edge = hit.left() + 3.0;
    let press = pos2(edge - 2.0, hit.center().y);
    drag(&mut h, press, &[press + vec2(20.0, 0.0), press + vec2(40.0, 0.0)]);
    match &h.state().layout.root {
        Some(Node::Split { size, .. }) => assert_eq!(*size, SplitSize::FixedFirst(260.0)),
        other => panic!("{other:?}"),
    }
}

#[test]
fn floating_only_layouts_leave_the_cursor_alone() {
    let layout = Layout { root: None, floating: vec![Floating { panels: vec![1, 2], active: 0, rect: [40.0, 40.0, 200.0, 160.0] }] };
    let mut h = Harness::builder().with_size(vec2(600.0, 400.0)).build_ui_state(
        |ui, d: &mut (L, Option<(Rect, Rect)>)| {
            let before = ui.available_rect_before_wrap();
            let style = DockStyle::from_ui(ui);
            let _ = DockArea::new(area()).show(
                ui,
                &d.0,
                &style,
                |p| format!("Panel {p}"),
                |_| Permissions::default(),
                |ui, _| {
                    ui.label("x");
                },
            );
            d.1 = Some((before, ui.available_rect_before_wrap()));
        },
        (layout, None),
    );
    h.step();
    let (before, after) = h.state().1.unwrap();
    assert_eq!(before, after);
}

#[test]
fn group_grip_moves_the_group_and_tabs_still_detach() {
    let mut h = harness(Dock::new(sample()), vec2(900.0, 600.0));
    let start = pos2(86.0, 82.0);
    let grip = rect_of(&h, area().with(("move-group", Some(5u32))));
    assert!(grip.contains(start), "{grip:?}");
    drag(&mut h, start, &[start + vec2(25.0, 20.0), start + vec2(50.0, 40.0)]);
    let g = &h.state().layout.floating[0];
    assert_eq!((g.panels.clone(), g.active), (vec![5, 6], 1));
    assert_ne!(g.rect, [80.0, 70.0, 300.0, 220.0]);
    assert!(h.state().actions.iter().all(|a| matches!(a, Action::MoveFloating { panel: 5, .. })));

    let t5 = rect_of(&h, tab(5)).center();
    drag(&mut h, t5, &[t5 + vec2(50.0, 50.0), t5 + vec2(100.0, 100.0)]);
    let s = h.state();
    assert!(s.actions.iter().any(|a| matches!(a, Action::Float { panel: 5, .. })), "{:?}", s.actions);
    assert_eq!(s.layout.floating.len(), 2);
    assert_eq!(s.layout.floating[0].panels, vec![6]);
}

#[test]
fn cancelling_a_group_drag_and_replacing_the_layout() {
    let mut h = harness(Dock::new(sample()), vec2(900.0, 600.0));
    let start = rect_of(&h, area().with(("move-group", Some(5u32)))).center();
    h.hover_at(start);
    h.step();
    h.drag_at(start);
    h.step();
    h.hover_at(start + vec2(10.0, 10.0));
    h.step();
    cancel_drag::<u32>(&h.ctx, area());
    h.state_mut().layout = sample();
    h.state_mut().actions.clear();
    h.state_mut().group_drags.clear();
    h.hover_at(start + vec2(60.0, 60.0));
    h.step();
    assert!(h.state().actions.is_empty(), "{:?}", h.state().actions);
    assert!(h.state().group_drags.is_empty());
    h.drop_at(start + vec2(60.0, 60.0));
    h.step();
    assert!(h.state().actions.is_empty());
    assert!(h.state().group_drops.is_empty());
}

/// Records what the renderer asked of a custom content adapter.
#[derive(Default)]
struct Custom {
    labels: Vec<String>,
    headers: usize,
    bodies: usize,
    menus: usize,
    visual_actions: bool,
}

impl DockContent<u32> for Custom {
    fn body(&mut self, ui: &mut Ui, panel: &u32) {
        self.bodies += 1;
        ui.label(format!("Body {panel}"));
    }

    fn label(&mut self, _ui: &Ui, _panel: &u32, logical: &str, font: &egui::FontId, color: egui::Color32) -> egui::WidgetText {
        self.labels.push(logical.to_owned());
        egui::RichText::new(logical).font(font.clone()).color(color).into()
    }

    fn action_label(&mut self, _ui: &Ui, logical: &str, font: &egui::FontId, color: egui::Color32) -> egui::WidgetText {
        let text = if self.visual_actions { format!("Action visual: {logical}") } else { logical.to_owned() };
        egui::RichText::new(text).font(font.clone()).color(color).into()
    }

    fn panel_menu(&mut self, ui: &mut Ui, _panel: &u32) {
        self.menus += 1;
        ui.label("custom entry");
    }

    fn group_header_width(&self, _panels: &[u32], _active: &u32, _floating: bool) -> f32 {
        40.0
    }

    fn group_header(&mut self, ui: &mut Ui, _panels: &[u32], _active: &u32, _floating: bool) {
        self.headers += 1;
        ui.label("H");
    }
}

struct CustomDock {
    layout: L,
    content: Custom,
    output: DockOutput<u32>,
    raise: Option<u32>,
}

fn custom_harness(layout: L, size: Vec2, scale: f32, visual: bool) -> Harness<'static, CustomDock> {
    let state = CustomDock { layout, content: Custom { visual_actions: visual, ..Custom::default() }, output: DockOutput::default(), raise: None };
    let mut h = Harness::builder().with_size(size).with_pixels_per_point(scale).build_ui_state(
        |ui, s: &mut CustomDock| {
            let style = DockStyle::from_ui(ui);
            let mut dock = DockArea::new(area());
            if let Some(p) = s.raise.take() {
                dock = dock.raise_panel(p);
            }
            s.output = dock.show_customized(
                ui,
                &s.layout,
                &style,
                |p| format!("Logical {p}"),
                |_| Permissions::default(),
                |_| PanelLimits::default(),
                &mut s.content,
            );
        },
        state,
    );
    h.step();
    h
}

#[test]
fn custom_content_hooks_are_called() {
    let mut h = custom_harness(sample(), vec2(900.0, 600.0), 1.0, false);
    h.state_mut().raise = Some(5);
    h.step();
    assert_eq!(h.state().output.focused, Some(5));
    assert!(h.state().output.actions.is_empty());
    h.step();
    let c = &h.state().content;
    assert!(c.headers >= 3 && c.bodies >= 3, "{} {}", c.headers, c.bodies);
    assert!(c.labels.iter().any(|l| l == "Logical 5"));
    let t5 = h.ctx.read_response(tab(5)).unwrap().rect.center();
    h.hover_at(t5);
    h.step();
    h.event(Event::PointerButton { pos: t5, button: PointerButton::Secondary, pressed: true, modifiers: Modifiers::NONE });
    h.step();
    h.event(Event::PointerButton { pos: t5, button: PointerButton::Secondary, pressed: false, modifiers: Modifiers::NONE });
    h.step();
    h.step();
    assert!(h.state().content.menus >= 1);
}

#[test]
fn a_blocker_under_the_release_blocks_drop_and_float() {
    let mut h = harness(Dock::new(sample()), vec2(900.0, 600.0));
    let from = rect_of(&h, tab(3)).center();
    let to = pos2(600.0, 500.0);
    h.hover_at(from);
    h.step();
    h.drag_at(from);
    h.step();
    h.hover_at(from + vec2(30.0, 30.0));
    h.step();
    h.state_mut().blockers = vec![Rect::from_center_size(to, vec2(100.0, 100.0))];
    h.hover_at(to);
    h.step();
    h.drop_at(to);
    h.step();
    h.step();
    assert!(h.state().actions.is_empty(), "{:?}", h.state().actions);

    // The same gesture without the blocker floats the panel.
    h.state_mut().blockers.clear();
    h.step();
    drag(&mut h, from, &[from + vec2(30.0, 30.0), to]);
    assert!(matches!(h.state().actions.as_slice(), [Action::Move { panel: 3, .. }]), "{:?}", h.state().actions);
}

#[test]
fn escape_restores_a_moved_group() {
    let mut h = harness(Dock::new(sample()), vec2(900.0, 600.0));
    let start = rect_of(&h, area().with(("move-group", Some(5u32)))).center();
    h.hover_at(start);
    h.step();
    h.drag_at(start);
    h.step();
    h.hover_at(start + vec2(40.0, 30.0));
    h.step();
    assert_ne!(h.state().layout.floating[0].rect, [80.0, 70.0, 300.0, 220.0]);
    h.key_press(Key::Escape);
    h.step();
    assert_eq!(h.state().layout.floating[0].rect, [80.0, 70.0, 300.0, 220.0]);
    h.drop_at(start + vec2(40.0, 30.0));
    h.step();
}

#[test]
fn crowded_tabs_render_at_every_scale() {
    for scale in [0.75, 1.0, 1.5, 2.0] {
        let layout = Layout { root: Some(tabs(&[1, 2, 3, 4, 5, 6], 5)), floating: vec![] };
        let mut h = custom_harness(layout, vec2(120.0 * scale, 160.0), scale, false);
        h.step();
        let s = h.state();
        assert_eq!(s.output.error, None);
        assert!(s.content.headers > 0 && s.content.bodies > 0);
        assert!(s.content.labels.iter().any(|l| l == "Logical 6"));
    }
}

#[test]
fn protected_members_pin_the_floating_group() {
    let mut dock = Dock::new(sample());
    dock.permissions = |p| if *p == 6 { Permissions::PROTECTED } else { Permissions::default() };
    let mut h = harness(dock, vec2(900.0, 600.0));
    let start = rect_of(&h, area().with(("move-group", Some(5u32)))).center();
    h.hover_at(start);
    h.step();
    h.drag_at(start);
    h.step();
    h.hover_at(start + vec2(40.0, 30.0));
    h.step();
    h.drop_at(start + vec2(40.0, 30.0));
    h.step();
    assert!(h.state().group_drags.is_empty() && h.state().group_drops.is_empty());
    assert!(h.state().actions.is_empty());

    // Control: with every member movable the same gesture reports the drag and the drop.
    h.state_mut().permissions = |_| Permissions::default();
    h.step();
    h.hover_at(start);
    h.step();
    h.drag_at(start);
    h.step();
    h.hover_at(start + vec2(40.0, 30.0));
    h.step();
    assert_eq!(h.state().group_drags.last(), Some(&5));
    h.drop_at(start + vec2(40.0, 30.0));
    h.step();
    assert!(matches!(h.state().group_drops.as_slice(), [(5, _)]), "{:?}", h.state().group_drops);
}

fn dock_area() -> DockArea<u32> {
    DockArea::new(area())
}

#[test]
fn chrome_focus_classification() {
    let mut h = Harness::builder().with_size(vec2(900.0, 600.0)).build_ui_state(
        |ui, s: &mut (L, String)| {
            let style = DockStyle::from_ui(ui);
            let (layout, text) = s;
            let _ = DockArea::new(area()).show(
                ui,
                layout,
                &style,
                |p| format!("Panel {p}"),
                |_| Permissions::default(),
                |ui, p| {
                    if *p == 1 {
                        ui.add(egui::TextEdit::singleline(text).id(Id::new("field")));
                    }
                },
            );
        },
        (sample(), String::new()),
    );
    h.step();
    h.ctx.memory_mut(|m| m.request_focus(tab(3)));
    h.step();
    assert!(dock_area().chrome_has_focus(&h.ctx, &h.state().0));
    assert!(!dock_area().chrome_has_focus(&h.ctx, &Layout { root: Some(tabs(&[3, 3], 0)), floating: vec![] }));
    assert!(!dock_area().chrome_has_focus(&h.ctx, &Layout::default()));
    h.ctx.memory_mut(|m| m.request_focus(Id::new("field")));
    h.step();
    assert!(!dock_area().chrome_has_focus(&h.ctx, &h.state().0));
    h.ctx.memory_mut(|m| m.request_focus(tab(42)));
    assert!(!dock_area().chrome_has_focus(&h.ctx, &Layout { root: Some(tabs(&[42], 0)), floating: vec![] }));
}

#[test]
fn chrome_focus_covers_every_control() {
    let layout = Layout {
        root: Some(split(
            SplitAxis::Horizontal,
            SplitSize::FixedFirst(300.0),
            Node::Stack { entries: vec![entry(1, true, None), entry(2, true, None)] },
            tabs(&[3], 0),
        )),
        floating: vec![Floating { panels: vec![5], active: 0, rect: [400.0, 300.0, 200.0, 150.0] }],
    };
    let mut h = harness(Dock::new(layout), vec2(900.0, 600.0));
    let ids = [
        tab(1),
        area().with(("collapse", 1u32)),
        area().with(("stack-resize", 1u32)),
        area().with(("split", Vec::<bool>::new())),
        area().with(("floating", 5u32)).with("resize"),
    ];
    for id in ids {
        h.ctx.memory_mut(|m| m.request_focus(id));
        h.step();
        h.step();
        assert!(dock_area().chrome_has_focus(&h.ctx, &h.state().layout), "{id:?}");
    }
}

#[test]
fn cancel_drag_stops_resize_gestures() {
    let layout = Layout {
        root: Some(split(
            SplitAxis::Horizontal,
            SplitSize::FixedFirst(300.0),
            Node::Stack { entries: vec![entry(1, true, Some(150.0)), entry(2, true, Some(150.0))] },
            tabs(&[3], 0),
        )),
        floating: vec![Floating { panels: vec![5], active: 0, rect: [400.0, 300.0, 200.0, 150.0] }],
    };
    let controls = [area().with(("split", Vec::<bool>::new())), area().with(("stack-resize", 1u32)), area().with(("floating", 5u32)).with("resize")];
    for id in controls {
        let mut h = harness(Dock::new(layout.clone()), vec2(900.0, 600.0));
        let c = rect_of(&h, id).center();
        h.hover_at(c);
        h.step();
        h.drag_at(c);
        h.step();
        assert_eq!(h.ctx.dragged_id(), Some(id));
        cancel_drag::<u32>(&h.ctx, area());
        assert_eq!(h.ctx.dragged_id(), None);
        let frozen = h.state().layout.clone();
        h.hover_at(c + vec2(10.0, 10.0));
        h.step();
        h.hover_at(c + vec2(20.0, 20.0));
        h.step();
        assert_eq!(h.state().layout, frozen, "{id:?}");
        h.drop_at(c + vec2(20.0, 20.0));
        h.step();
        let c = rect_of(&h, id).center();
        drag(&mut h, c, &[c + vec2(10.0, 10.0)]);
        assert_ne!(h.state().layout, frozen, "{id:?}");
    }
}

#[test]
fn cancelling_one_area_leaves_other_gestures_alone() {
    struct Two {
        left: L,
        right: L,
        text: String,
        value: f32,
        changes: usize,
    }
    let pair = || Layout { root: Some(split(SplitAxis::Horizontal, SplitSize::FixedFirst(150.0), tabs(&[1], 0), tabs(&[2], 0))), floating: vec![] };
    let mut h = Harness::builder().with_size(vec2(800.0, 400.0)).build_ui_state(
        |ui, s: &mut Two| {
            let style = DockStyle::from_ui(ui);
            ui.columns(2, |cols| {
                let out =
                    DockArea::new(Id::new("left")).show(&mut cols[0], &s.left, &style, |p| format!("L{p}"), |_| Permissions::default(), |_, _| {});
                for a in out.actions {
                    s.left.apply(a).unwrap();
                    s.changes += 1;
                }
                let (text, value) = (&mut s.text, &mut s.value);
                let out = DockArea::new(Id::new("right")).show(
                    &mut cols[1],
                    &s.right,
                    &style,
                    |p| format!("R{p}"),
                    |_| Permissions::default(),
                    |ui, p| {
                        if *p == 1 {
                            ui.add(egui::Slider::new(value, 0.0..=100.0).text("slider"));
                            ui.add(egui::TextEdit::singleline(text));
                        }
                    },
                );
                for a in out.actions {
                    s.right.apply(a).unwrap();
                    s.changes += 1;
                }
            });
        },
        Two { left: pair(), right: pair(), text: String::new(), value: 0.0, changes: 0 },
    );
    h.step();
    let right_split = Id::new("right").with(("split", Vec::<bool>::new()));
    let c = rect_of_ctx(&h.ctx, right_split).center();
    h.hover_at(c);
    h.step();
    h.drag_at(c);
    h.step();
    h.hover_at(c + vec2(10.0, 0.0));
    h.step();
    cancel_drag::<u32>(&h.ctx, Id::new("left"));
    assert_eq!(h.ctx.dragged_id(), Some(right_split));
    let before = h.state().changes;
    h.hover_at(c + vec2(30.0, 0.0));
    h.step();
    assert!(h.state().changes > before);
    h.drop_at(c + vec2(30.0, 0.0));
    h.step();
    h.step();

    // A drag on a widget inside a panel body is never stopped, not even by its own area.
    let slider = h.get_by_role(egui::accesskit::Role::Slider).rect().center();
    h.hover_at(slider);
    h.step();
    h.drag_at(slider);
    h.step();
    h.hover_at(slider + vec2(10.0, 0.0));
    h.step();
    let dragged = h.ctx.dragged_id();
    assert!(dragged.is_some());
    cancel_drag::<u32>(&h.ctx, Id::new("right"));
    cancel_drag::<u32>(&h.ctx, Id::new("left"));
    assert_eq!(h.ctx.dragged_id(), dragged);
    let before = h.state().value;
    h.hover_at(slider + vec2(40.0, 0.0));
    h.step();
    assert!(h.state().value > before, "{before} {}", h.state().value);
    h.drop_at(slider + vec2(40.0, 0.0));
    h.step();
}

fn rect_of_ctx(ctx: &egui::Context, id: Id) -> Rect {
    ctx.read_response(id).map(|r| r.rect).unwrap_or_else(|| panic!("{id:?} not rendered"))
}

#[test]
fn stock_menu_actions_keep_logical_accessible_names() {
    let mut h = custom_harness(sample(), vec2(900.0, 600.0), 1.0, true);
    let t5 = h.ctx.read_response(tab(5)).unwrap().rect.center();
    h.hover_at(t5);
    h.step();
    h.event(Event::PointerButton { pos: t5, button: PointerButton::Secondary, pressed: true, modifiers: Modifiers::NONE });
    h.step();
    h.event(Event::PointerButton { pos: t5, button: PointerButton::Secondary, pressed: false, modifiers: Modifiers::NONE });
    h.step();
    h.step();
    assert_eq!(h.query_all_by_label("Move to group").count(), 2);
    assert_eq!(h.query_all_by_label("Float panel").count(), 1);
    assert_eq!(h.query_all_by_label("Close panel").count(), 1);
    for logical in ["Float panel", "Move to group", "Close panel"] {
        assert!(h.query_by_label_contains(&format!("Action visual: {logical}")).is_none());
    }
    let shapes = format!("{:?}", h.output().shapes);
    for logical in ["Float panel", "Move to group", "Close panel"] {
        assert!(shapes.contains(&format!("Action visual: {logical}")), "{logical} not painted");
    }
    let node = h.get_by_label("Float panel");
    assert!(!node.accesskit_node().is_disabled());
}
