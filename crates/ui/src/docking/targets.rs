//! Turning a pointer position over rendered groups into a placement.
//!
//! These functions know nothing about egui widgets: any chrome that can describe its groups as
//! [`DropTarget`]s gets the same drop rules as the built-in renderer.

use egui::{Pos2, Rect, pos2};

use super::model::{MAX_PANELS, Placement, Zone};
use crate::layout::usable;

/// One rendered group a panel could be dropped on.
#[derive(Clone, Debug)]
pub struct DropTarget<P> {
    /// The panel a drop here is placed against (normally the group's selected panel).
    pub anchor: P,
    /// The whole group, tab strip included.
    pub rect: Rect,
    /// The tab strip, if the group has one; dropping inside it joins the tabs.
    pub tab_strip: Option<Rect>,
    /// The group's tabs in order with their rects (`Rect::NOTHING` for hidden ones).
    pub tabs: Vec<(P, Rect)>,
    /// Whether side zones may split this group.
    pub allow_splits: bool,
    /// Whether panels may join this group's tabs.
    pub accepts_tabs: bool,
}

/// What a drop at the pointer would do, and where to draw it.
#[derive(Clone, Debug, PartialEq)]
pub struct DropProposal<P> {
    pub anchor: P,
    pub placement: Placement<P>,
    /// The area the panel would occupy.
    pub preview: Rect,
    /// A thin marker where a tab would be inserted.
    pub insertion: Option<Rect>,
}

/// Distance (as a fraction of the group size) within which an edge zone wins over the centre.
const EDGE: f32 = 0.25;

/// The zone of `rect` under `pointer`. The top `tab_height` points (the tab strip) are always
/// the centre; elsewhere the nearest edge wins within a quarter of the size, ties going to Left,
/// Right, Top, Bottom in that order.
pub fn drop_zone(rect: Rect, tab_height: f32, pointer: Pos2) -> Option<Zone> {
    if !usable(rect) || !pointer.is_finite() || !rect.contains(pointer) || !tab_height.is_finite() || tab_height < 0.0 {
        return None;
    }
    if pointer.y < rect.top() + tab_height.min(rect.height()) {
        return Some(Zone::Center);
    }
    let x = (pointer.x - rect.left()) / rect.width();
    let y = (pointer.y - rect.top()) / rect.height();
    let candidates = [(Zone::Left, x), (Zone::Right, 1.0 - x), (Zone::Top, y), (Zone::Bottom, 1.0 - y)];
    let mut best = (Zone::Left, x);
    for candidate in candidates {
        if candidate.1 < best.1 {
            best = candidate;
        }
    }
    Some(if best.1 > EDGE { Zone::Center } else { best.0 })
}

/// The part of `rect` a panel dropped in `zone` would take: all of it for the centre, else the
/// matching half.
pub fn zone_rect(rect: Rect, zone: Zone) -> Option<Rect> {
    if !usable(rect) {
        return None;
    }
    let c = rect.center();
    Some(match zone {
        Zone::Center => rect,
        Zone::Left => Rect::from_min_max(rect.min, pos2(c.x, rect.max.y)),
        Zone::Right => Rect::from_min_max(pos2(c.x, rect.min.y), rect.max),
        Zone::Top => Rect::from_min_max(rect.min, pos2(rect.max.x, c.y)),
        Zone::Bottom => Rect::from_min_max(pos2(rect.min.x, c.y), rect.max),
    })
}

/// Resolve dropping `panel` at `pointer` over `targets` (listed back to front).
///
/// The frontmost target containing the pointer decides alone: if it rejects the drop, nothing
/// behind it is considered.
pub fn resolve_drop<P: Clone + Eq>(panel: &P, pointer: Pos2, targets: &[DropTarget<P>]) -> Option<DropProposal<P>> {
    if !pointer.is_finite() || targets.len() > MAX_PANELS {
        return None;
    }
    let target = targets.iter().rev().find(|t| usable(t.rect) && t.rect.contains(pointer))?;
    if target.tabs.len() > MAX_PANELS {
        return None;
    }
    let anchor = if target.anchor == *panel { target.tabs.iter().map(|(p, _)| p).find(|p| *p != panel)?.clone() } else { target.anchor.clone() };

    if let Some(strip) = target.tab_strip.filter(|s| usable(*s) && s.contains(pointer)) {
        if !target.accepts_tabs {
            return None;
        }
        let visible = || target.tabs.iter().filter(|(_, r)| usable(*r));
        let next = visible().find(|(_, r)| r.center().x > pointer.x);
        let x = match next {
            Some((_, r)) => r.left(),
            None => visible().map(|(_, r)| r.right()).fold(strip.left(), f32::max),
        }
        .min(strip.right());
        let left = (x - 1.0).clamp(strip.left(), strip.right());
        let right = (x + 1.0).clamp(strip.left(), strip.right());
        let insertion = Rect::from_min_max(pos2(left, strip.top()), pos2(right, strip.bottom()));
        return Some(DropProposal {
            anchor,
            placement: Placement::Tab { before: next.map(|(p, _)| p.clone()) },
            preview: target.rect,
            insertion: Some(insertion),
        });
    }

    let zone = if target.allow_splits { drop_zone(target.rect, 0.0, pointer)? } else { Zone::Center };
    match zone {
        Zone::Center if !target.accepts_tabs || target.tabs.iter().any(|(p, _)| p == panel) => return None,
        Zone::Center => {}
        _ if !target.allow_splits => return None,
        _ => {}
    }
    Some(DropProposal { anchor, placement: Placement::Split(zone), preview: zone_rect(target.rect, zone)?, insertion: None })
}
