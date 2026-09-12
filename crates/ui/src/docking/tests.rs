use egui::{Rect, pos2, vec2};

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
