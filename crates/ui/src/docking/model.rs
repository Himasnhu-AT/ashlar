//! The panel layout tree, its validation, and the transactions that change it.
//!
//! Every mutation goes through [`Layout::apply_with_permissions`] (or [`Layout::restore`]): the
//! current layout is validated, the change is made on a copy, the copy is validated, and only then
//! is it committed. A failed transaction leaves the layout exactly as it was.

use std::collections::HashSet;
use std::hash::Hash;

use crate::layout::{EXTENT, SplitAxis, SplitSize};

/// Deepest split nesting a layout may have (the root node is depth 1).
pub const MAX_DEPTH: usize = 64;
/// Most panels (and most floating groups) a layout may hold.
pub const MAX_PANELS: usize = 4096;
/// Most nodes a traversal visits before giving up.
pub(crate) const MAX_NODES: usize = 2 * MAX_PANELS;

/// Where a dragged panel lands relative to a group: into its tabs, or beside it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Zone {
    Center,
    Left,
    Right,
    Top,
    Bottom,
}

/// One node of the docked tree.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Node<P> {
    /// Two children side by side (`Horizontal`) or stacked (`Vertical`).
    Split { axis: SplitAxis, size: SplitSize, first: Box<Node<P>>, second: Box<Node<P>> },
    /// A tab group: ordered panels, one of them shown.
    Tabs { panels: Vec<P>, active: usize },
    /// An accordion: every entry has a title row; open entries also show their body.
    Stack { entries: Vec<StackEntry<P>> },
}

/// One accordion entry.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct StackEntry<P> {
    pub panel: P,
    pub open: bool,
    /// Preferred body height; `None` uses the renderer's minimum pane size.
    pub height: Option<f32>,
}

/// A tab group floating above the docked tree. `rect` is `[x, y, width, height]`.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Floating<P> {
    pub panels: Vec<P>,
    pub active: usize,
    pub rect: [f32; 4],
}

/// The whole arrangement: an optional docked tree plus floating groups (back to front).
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Layout<P> {
    pub root: Option<Node<P>>,
    pub floating: Vec<Floating<P>>,
}

impl<P> Default for Layout<P> {
    fn default() -> Self {
        Self { root: None, floating: Vec::new() }
    }
}

/// How a panel is placed next to an anchor panel.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Placement<P> {
    /// Into the anchor's group, before `before` (or at the end).
    Tab { before: Option<P> },
    /// `Center` joins the anchor's group right after it; the sides split the group.
    Split(Zone),
}

/// Where a panel was, captured by [`Layout::location`] so [`Layout::restore`] can put it back.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(bound(deserialize = "P: serde::Deserialize<'de>")))]
pub struct Location<P> {
    /// The floating group's rect, when the panel was floating.
    pub floating: Option<[f32; 4]>,
    /// A neighbour to place the panel against.
    pub anchor: Option<P>,
    pub placement: Placement<P>,
    /// The size of the split the panel had to itself.
    pub size: Option<SplitSize>,
    /// `(open, height)` when the panel was an accordion entry.
    pub stack: Option<(bool, Option<f32>)>,
    /// Every panel on the other side of that split, to find the same subtree again.
    #[cfg_attr(feature = "serde", serde(default))]
    pub sibling: Vec<P>,
}

/// A change to a [`Layout`], as produced by the renderer or by app commands.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Action<P> {
    Move { panel: P, anchor: P, placement: Placement<P> },
    Float { panel: P, rect: [f32; 4] },
    Close { panel: P },
    Activate { panel: P },
    Open { panel: P, anchor: Option<P> },
    OpenAt { panel: P, anchor: P, placement: Placement<P> },
    MoveFloating { panel: P, rect: [f32; 4] },
    ResizeSplit { path: Vec<bool>, size: SplitSize },
    SetStackOpen { panel: P, open: bool },
    ResizeStack { panel: P, height: Option<f32> },
}

/// What the user may do with a panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Permissions {
    pub movable: bool,
    pub floatable: bool,
    pub closable: bool,
    /// Whether other panels may join this panel's group as tabs.
    pub accepts_tabs: bool,
}

impl Default for Permissions {
    fn default() -> Self {
        Self { movable: true, floatable: true, closable: true, accepts_tabs: true }
    }
}

impl Permissions {
    /// Nothing allowed: a fixed panel such as a document canvas.
    pub const PROTECTED: Self = Self { movable: false, floatable: false, closable: false, accepts_tabs: false };
}

/// Why a layout or a transaction was rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DockError {
    #[error("the layout exceeds its size or depth limit")]
    LimitExceeded,
    #[error("a panel appears more than once in the layout")]
    DuplicatePanel,
    #[error("the panel is not in the layout")]
    MissingPanel,
    #[error("the target of the change is no longer in the layout")]
    MissingTarget,
    #[error("an active index points past the end of its group")]
    InvalidSelection,
    #[error("a size or rect is not finite or out of range")]
    InvalidGeometry,
    #[error("the target cannot take the panel that way")]
    InvalidPlacement,
    #[error("the panel's permissions do not allow this")]
    NotAllowed,
}

// ---------------------------------------------------------------------------------------------
// Value checks

pub(crate) fn rect_ok(r: [f32; 4]) -> bool {
    r.iter().all(|v| v.is_finite() && v.abs() <= EXTENT) && r[2] > 0.0 && r[3] > 0.0
}

pub(crate) fn size_ok(size: SplitSize) -> bool {
    match size {
        SplitSize::Ratio(r) => r.is_finite() && (0.0..=1.0).contains(&r),
        SplitSize::FixedFirst(v) | SplitSize::FixedSecond(v) => v.is_finite() && (0.0..=EXTENT).contains(&v),
    }
}

pub(crate) fn height_ok(height: Option<f32>) -> bool {
    height.is_none_or(|h| h.is_finite() && (0.0..=EXTENT).contains(&h))
}

fn active_ok(active: usize, len: usize) -> bool {
    active < len || (len == 0 && active == 0)
}

/// Registers panel ids in one shared set while validating.
struct Seen<'a, P> {
    ids: HashSet<&'a P>,
}

impl<'a, P: Eq + Hash> Seen<'a, P> {
    fn add(&mut self, panel: &'a P) -> Result<(), DockError> {
        if self.ids.len() >= MAX_PANELS {
            return Err(DockError::LimitExceeded);
        }
        if !self.ids.insert(panel) {
            return Err(DockError::DuplicatePanel);
        }
        Ok(())
    }

    fn tabs(&mut self, panels: &'a [P], active: usize) -> Result<(), DockError> {
        if panels.len() > MAX_PANELS {
            return Err(DockError::LimitExceeded);
        }
        if !active_ok(active, panels.len()) {
            return Err(DockError::InvalidSelection);
        }
        panels.iter().try_for_each(|p| self.add(p))
    }
}

/// Where a panel sits.
enum Spot {
    /// In a root group, reached by this split path.
    Root(Vec<bool>),
    /// In the floating group at this index.
    Floating(usize),
}

// ---------------------------------------------------------------------------------------------
// Queries

impl<P: Clone + Eq + Hash> Layout<P> {
    /// Check limits, uniqueness, selections and geometry without recursion.
    pub fn validate(&self) -> Result<(), DockError> {
        if self.floating.len() > MAX_PANELS {
            return Err(DockError::LimitExceeded);
        }
        let mut seen = Seen { ids: HashSet::new() };
        let mut stack: Vec<(&Node<P>, usize)> = self.root.iter().map(|n| (n, 1)).collect();
        let mut visited = 0usize;
        while let Some((node, depth)) = stack.pop() {
            visited += 1;
            if depth > MAX_DEPTH || visited > MAX_NODES {
                return Err(DockError::LimitExceeded);
            }
            match node {
                Node::Split { size, first, second, .. } => {
                    if !size_ok(*size) {
                        return Err(DockError::InvalidGeometry);
                    }
                    // Second pushed first so the first child is checked first.
                    stack.push((second, depth + 1));
                    stack.push((first, depth + 1));
                }
                Node::Tabs { panels, active } => seen.tabs(panels, *active)?,
                Node::Stack { entries } => {
                    if entries.len() > MAX_PANELS {
                        return Err(DockError::LimitExceeded);
                    }
                    for entry in entries {
                        if !height_ok(entry.height) {
                            return Err(DockError::InvalidGeometry);
                        }
                        seen.add(&entry.panel)?;
                    }
                }
            }
        }
        for group in &self.floating {
            if !rect_ok(group.rect) || group.panels.is_empty() {
                return Err(DockError::InvalidGeometry);
            }
            seen.tabs(&group.panels, group.active)?;
        }
        Ok(())
    }

    /// Every panel: the docked tree in pre-order, then the floating groups. At most
    /// [`MAX_PANELS`] are returned.
    pub fn panels(&self) -> Vec<&P> {
        let mut out = Vec::new();
        if let Some(root) = &self.root {
            collect(root, &mut out);
        }
        for group in &self.floating {
            for p in &group.panels {
                if out.len() >= MAX_PANELS {
                    return out;
                }
                out.push(p);
            }
        }
        out
    }

    pub fn contains(&self, panel: &P) -> bool {
        self.panels().contains(&panel)
    }

    /// Where `panel` is, in a form [`Self::restore`] can use after it has been closed.
    pub fn location(&self, panel: &P) -> Result<Location<P>, DockError> {
        self.validate()?;
        match self.find(panel).ok_or(DockError::MissingPanel)? {
            Spot::Floating(i) => {
                let group = self.floating.get(i).ok_or(DockError::MissingPanel)?;
                let (anchor, before) = neighbours(&group.panels, panel);
                Ok(Location {
                    floating: Some(group.rect),
                    anchor,
                    placement: Placement::Tab { before },
                    size: None,
                    stack: None,
                    sibling: Vec::new(),
                })
            }
            Spot::Root(path) => {
                let root = self.root.as_ref().ok_or(DockError::MissingPanel)?;
                let group = node_at(root, &path).ok_or(DockError::MissingPanel)?;
                let mut loc = Location {
                    floating: None,
                    anchor: None,
                    placement: Placement::Tab { before: None },
                    size: None,
                    stack: None,
                    sibling: Vec::new(),
                };
                match group {
                    Node::Tabs { panels, .. } => {
                        let (anchor, before) = neighbours(panels, panel);
                        loc.anchor = anchor;
                        loc.placement = Placement::Tab { before };
                    }
                    Node::Stack { entries } => {
                        let ids: Vec<P> = entries.iter().map(|e| e.panel.clone()).collect();
                        let (anchor, before) = neighbours(&ids, panel);
                        loc.anchor = anchor;
                        loc.placement = Placement::Tab { before };
                        loc.stack = entries.iter().find(|e| &e.panel == panel).map(|e| (e.open, e.height));
                    }
                    Node::Split { .. } => return Err(DockError::MissingPanel),
                }
                if loc.anchor.is_none() {
                    // Alone in its group: describe it by the innermost split around it.
                    for depth in (0..path.len()).rev() {
                        let (prefix, side) = (path.get(..depth).unwrap_or(&[]), path.get(depth).copied().unwrap_or(false));
                        let Some(Node::Split { axis, size, first, second }) = node_at(root, prefix) else { continue };
                        let other = if side { first } else { second };
                        let mut sibling = Vec::new();
                        collect(other, &mut sibling);
                        let Some(anchor) = sibling.first() else { continue };
                        loc.anchor = Some((*anchor).clone());
                        loc.placement = Placement::Split(match (axis, side) {
                            (SplitAxis::Horizontal, false) => Zone::Left,
                            (SplitAxis::Horizontal, true) => Zone::Right,
                            (SplitAxis::Vertical, false) => Zone::Top,
                            (SplitAxis::Vertical, true) => Zone::Bottom,
                        });
                        loc.size = Some(*size);
                        loc.sibling = sibling.into_iter().cloned().collect();
                        break;
                    }
                }
                Ok(loc)
            }
        }
    }

    fn find(&self, panel: &P) -> Option<Spot> {
        if let Some(i) = self.floating.iter().position(|g| g.panels.contains(panel)) {
            return Some(Spot::Floating(i));
        }
        let root = self.root.as_ref()?;
        let mut stack: Vec<(&Node<P>, Vec<bool>)> = vec![(root, Vec::new())];
        let mut visited = 0usize;
        while let Some((node, path)) = stack.pop() {
            visited += 1;
            if visited > MAX_NODES || path.len() >= MAX_DEPTH {
                return None;
            }
            match node {
                Node::Split { first, second, .. } => {
                    let mut b = path.clone();
                    b.push(true);
                    stack.push((second, b));
                    let mut a = path;
                    a.push(false);
                    stack.push((first, a));
                }
                Node::Tabs { panels, .. } if panels.contains(panel) => return Some(Spot::Root(path)),
                Node::Stack { entries } if entries.iter().any(|e| &e.panel == panel) => return Some(Spot::Root(path)),
                _ => {}
            }
        }
        None
    }

    /// The ids of the group holding `panel` (root group or floating group).
    fn group_of(&self, panel: &P) -> Option<Vec<P>> {
        match self.find(panel)? {
            Spot::Floating(i) => self.floating.get(i).map(|g| g.panels.clone()),
            Spot::Root(path) => match node_at(self.root.as_ref()?, &path)? {
                Node::Tabs { panels, .. } => Some(panels.clone()),
                Node::Stack { entries } => Some(entries.iter().map(|e| e.panel.clone()).collect()),
                Node::Split { .. } => None,
            },
        }
    }

    fn same_group(&self, a: &P, b: &P) -> bool {
        self.group_of(a).is_some_and(|g| g.contains(b))
    }
}

/// Pre-order panel ids of a subtree, bounded like [`Layout::panels`].
fn collect<'a, P>(node: &'a Node<P>, out: &mut Vec<&'a P>) {
    let mut stack = vec![node];
    let mut visited = 0usize;
    while let Some(node) = stack.pop() {
        visited += 1;
        if visited > MAX_NODES {
            return;
        }
        match node {
            Node::Split { first, second, .. } => {
                stack.push(second);
                stack.push(first);
            }
            Node::Tabs { panels, .. } => {
                for p in panels {
                    if out.len() >= MAX_PANELS {
                        return;
                    }
                    out.push(p);
                }
            }
            Node::Stack { entries } => {
                for e in entries {
                    if out.len() >= MAX_PANELS {
                        return;
                    }
                    out.push(&e.panel);
                }
            }
        }
    }
}

/// The first other member of a group, and the member right after `panel`.
fn neighbours<P: Clone + Eq>(group: &[P], panel: &P) -> (Option<P>, Option<P>) {
    let anchor = group.iter().find(|p| *p != panel).cloned();
    let before = group.iter().position(|p| p == panel).and_then(|i| group.get(i + 1)).cloned();
    (anchor, before)
}

fn node_at<'a, P>(root: &'a Node<P>, path: &[bool]) -> Option<&'a Node<P>> {
    let mut node = root;
    for &side in path {
        match node {
            Node::Split { first, second, .. } => node = if side { second } else { first },
            _ => return None,
        }
    }
    Some(node)
}

fn node_at_mut<'a, P>(root: &'a mut Node<P>, path: &[bool]) -> Option<&'a mut Node<P>> {
    let mut node = root;
    for &side in path {
        match node {
            Node::Split { first, second, .. } => node = if side { second } else { first },
            _ => return None,
        }
    }
    Some(node)
}

// ---------------------------------------------------------------------------------------------
// Transactions

impl<P: Clone + Eq + Hash> Layout<P> {
    /// Apply `action` with every permission granted.
    pub fn apply(&mut self, action: Action<P>) -> Result<(), DockError> {
        self.apply_with_permissions(action, |_| Permissions::default())
    }

    /// Apply `action` if `permissions` allow it. On any error the layout is unchanged.
    pub fn apply_with_permissions(&mut self, action: Action<P>, permissions: impl Fn(&P) -> Permissions) -> Result<(), DockError> {
        self.validate()?;
        check_permissions(&action, &permissions)?;
        let mut next = self.clone();
        next.perform(action)?;
        next.validate()?;
        *self = next;
        Ok(())
    }

    /// Put a closed panel back where [`Self::location`] said it was. Atomic like `apply`.
    pub fn restore(&mut self, panel: P, location: &Location<P>) -> Result<(), DockError> {
        self.validate()?;
        if self.contains(&panel) {
            return Err(DockError::DuplicatePanel);
        }
        if location.size.is_some_and(|s| !size_ok(s)) || location.stack.is_some_and(|(_, h)| !height_ok(h)) {
            return Err(DockError::InvalidGeometry);
        }
        let mut next = self.clone();
        next.restore_into(panel, location)?;
        next.validate()?;
        *self = next;
        Ok(())
    }

    fn restore_into(&mut self, panel: P, loc: &Location<P>) -> Result<(), DockError> {
        if let Some(rect) = loc.floating {
            if !rect_ok(rect) {
                return Err(DockError::InvalidGeometry);
            }
            match &loc.anchor {
                Some(anchor) => {
                    if !self.floating.iter().any(|g| g.panels.contains(anchor)) {
                        return Err(DockError::MissingTarget);
                    }
                    self.perform(Action::OpenAt { panel, anchor: anchor.clone(), placement: loc.placement.clone() })?;
                }
                None => self.floating.push(Floating { panels: vec![panel], active: 0, rect }),
            }
            return Ok(());
        }

        match &loc.anchor {
            Some(anchor) => {
                if !self.contains(anchor) {
                    return Err(DockError::MissingTarget);
                }
                match (&loc.placement, loc.sibling.is_empty()) {
                    (Placement::Split(zone), false) if *zone != Zone::Center => {
                        self.wrap_sibling(panel.clone(), *zone, loc)?;
                    }
                    _ => {
                        self.perform(Action::OpenAt { panel: panel.clone(), anchor: anchor.clone(), placement: loc.placement.clone() })?;
                        if let (Some(size), Some(root)) = (loc.size, self.root.as_mut()) {
                            set_size_beside(root, &panel, size);
                        }
                    }
                }
            }
            None => match self.root {
                None => self.root = Some(Node::Tabs { panels: vec![panel.clone()], active: 0 }),
                Some(_) => return Err(DockError::MissingTarget),
            },
        }

        if let Some((open, height)) = loc.stack
            && let Some(root) = self.root.as_mut()
        {
            tabs_to_stack(root, &panel);
            if let Some(entry) = stack_entry_mut(root, &panel) {
                entry.open = open;
                entry.height = height;
            }
        }
        Ok(())
    }

    /// Split the subtree holding exactly `loc.sibling`, putting `panel` on the `zone` side.
    fn wrap_sibling(&mut self, panel: P, zone: Zone, loc: &Location<P>) -> Result<(), DockError> {
        if loc.sibling.len() > MAX_PANELS {
            return Err(DockError::LimitExceeded);
        }
        let wanted: HashSet<&P> = loc.sibling.iter().collect();
        if wanted.len() != loc.sibling.len() || loc.anchor.as_ref().is_none_or(|a| !wanted.contains(a)) {
            return Err(DockError::InvalidPlacement);
        }
        let root = self.root.as_mut().ok_or(DockError::MissingTarget)?;
        let path = subtree_with(root, &wanted).ok_or(DockError::MissingTarget)?;
        let target = node_at_mut(root, &path).ok_or(DockError::MissingTarget)?;
        let size = loc.size.unwrap_or(SplitSize::Ratio(0.5));
        wrap(target, panel_node(panel), zone, size);
        Ok(())
    }

    fn perform(&mut self, action: Action<P>) -> Result<(), DockError> {
        match action {
            Action::Move { panel, anchor, placement } => self.move_panel(panel, anchor, placement),
            Action::Float { panel, rect } => {
                if !rect_ok(rect) {
                    return Err(DockError::InvalidGeometry);
                }
                self.take(&panel).ok_or(DockError::MissingPanel)?;
                self.floating.push(Floating { panels: vec![panel], active: 0, rect });
                Ok(())
            }
            Action::Close { panel } => self.take(&panel).map(|_| ()).ok_or(DockError::MissingPanel),
            Action::Activate { panel } => self.activate(&panel),
            Action::Open { panel, anchor } => {
                if self.contains(&panel) {
                    return self.activate(&panel);
                }
                let entry = StackEntry { panel: panel.clone(), open: true, height: None };
                match anchor {
                    Some(anchor) => self.insert(entry, &anchor, &Placement::Tab { before: None }),
                    None if self.root.is_none() => {
                        self.root = Some(panel_node(panel));
                        Ok(())
                    }
                    None => Err(DockError::MissingTarget),
                }
            }
            Action::OpenAt { panel, anchor, placement } => {
                if self.contains(&panel) {
                    return self.move_panel(panel, anchor, placement);
                }
                if let Placement::Tab { before: Some(b) } = &placement
                    && !self.same_group(b, &anchor)
                {
                    return Err(DockError::MissingTarget);
                }
                let entry = StackEntry { panel, open: true, height: None };
                self.insert(entry, &anchor, &placement)
            }
            Action::MoveFloating { panel, rect } => {
                if !rect_ok(rect) {
                    return Err(DockError::InvalidGeometry);
                }
                let group = self.floating.iter_mut().find(|g| g.panels.contains(&panel)).ok_or(DockError::MissingPanel)?;
                group.rect = rect;
                Ok(())
            }
            Action::ResizeSplit { path, size } => {
                if path.len() >= MAX_DEPTH {
                    return Err(DockError::LimitExceeded);
                }
                if !size_ok(size) {
                    return Err(DockError::InvalidGeometry);
                }
                let root = self.root.as_mut().ok_or(DockError::MissingTarget)?;
                match node_at_mut(root, &path) {
                    Some(Node::Split { size: s, .. }) => {
                        *s = size;
                        Ok(())
                    }
                    _ => Err(DockError::MissingTarget),
                }
            }
            Action::SetStackOpen { panel, open } => {
                let root = self.root.as_mut().ok_or(DockError::MissingPanel)?;
                stack_entry_mut(root, &panel).ok_or(DockError::MissingPanel)?.open = open;
                Ok(())
            }
            Action::ResizeStack { panel, height } => {
                if !height_ok(height) {
                    return Err(DockError::InvalidGeometry);
                }
                let root = self.root.as_mut().ok_or(DockError::MissingPanel)?;
                stack_entry_mut(root, &panel).ok_or(DockError::MissingPanel)?.height = height;
                Ok(())
            }
        }
    }

    fn move_panel(&mut self, panel: P, anchor: P, placement: Placement<P>) -> Result<(), DockError> {
        if panel == anchor {
            return Err(DockError::InvalidPlacement);
        }
        if !self.contains(&anchor) {
            return Err(DockError::MissingTarget);
        }
        if let Placement::Tab { before: Some(b) } = &placement {
            if *b == panel {
                return if self.same_group(&panel, &anchor) { Ok(()) } else { Err(DockError::InvalidPlacement) };
            }
            if !self.same_group(b, &anchor) {
                return Err(DockError::MissingTarget);
            }
        }
        let entry = self.take(&panel).ok_or(DockError::MissingPanel)?;
        self.insert(entry, &anchor, &placement)
    }

    fn activate(&mut self, panel: &P) -> Result<(), DockError> {
        if let Some(group) = self.floating.iter_mut().find(|g| g.panels.contains(panel)) {
            group.active = group.panels.iter().position(|p| p == panel).unwrap_or(group.active);
            return Ok(());
        }
        let Some(Spot::Root(path)) = self.find(panel) else { return Err(DockError::MissingPanel) };
        let root = self.root.as_mut().ok_or(DockError::MissingPanel)?;
        match node_at_mut(root, &path) {
            Some(Node::Tabs { panels, active }) => {
                *active = panels.iter().position(|p| p == panel).unwrap_or(*active);
                Ok(())
            }
            Some(Node::Stack { entries }) => {
                if let Some(e) = entries.iter_mut().find(|e| &e.panel == panel) {
                    e.open = true;
                }
                Ok(())
            }
            _ => Err(DockError::MissingPanel),
        }
    }

    /// Remove `panel` from wherever it is and return its entry.
    fn take(&mut self, panel: &P) -> Option<StackEntry<P>> {
        match self.find(panel)? {
            Spot::Floating(i) => {
                let group = self.floating.get_mut(i)?;
                let index = group.panels.iter().position(|p| p == panel)?;
                group.panels.remove(index);
                group.active = active_after_removal(group.active, index, group.panels.len());
                if group.panels.is_empty() {
                    self.floating.remove(i);
                }
                Some(StackEntry { panel: panel.clone(), open: true, height: None })
            }
            Spot::Root(_) => {
                let mut found = None;
                let root = self.root.take()?;
                self.root = take_from(root, panel, &mut found);
                found
            }
        }
    }

    /// Insert `entry` next to `anchor` as `placement` says.
    fn insert(&mut self, entry: StackEntry<P>, anchor: &P, placement: &Placement<P>) -> Result<(), DockError> {
        if let Some(group) = self.floating.iter_mut().find(|g| g.panels.contains(anchor)) {
            let index = match placement {
                Placement::Split(Zone::Center) => group.panels.iter().position(|p| p == anchor).map_or(group.panels.len(), |i| i + 1),
                Placement::Tab { before } => before.as_ref().and_then(|b| group.panels.iter().position(|p| p == b)).unwrap_or(group.panels.len()),
                Placement::Split(_) => return Err(DockError::InvalidPlacement),
            };
            group.panels.insert(index, entry.panel);
            group.active = index;
            return Ok(());
        }
        let Some(Spot::Root(path)) = self.find(anchor) else { return Err(DockError::MissingTarget) };
        let root = self.root.as_mut().ok_or(DockError::MissingTarget)?;
        let group = node_at_mut(root, &path).ok_or(DockError::MissingTarget)?;
        match placement {
            Placement::Split(zone) if *zone != Zone::Center => {
                wrap(group, panel_node(entry.panel), *zone, SplitSize::Ratio(0.5));
                Ok(())
            }
            _ => {
                // Only `Split(Center)` and `Tab` get here: the centre goes right after the anchor.
                let index_in = |ids: &[&P]| match placement {
                    Placement::Tab { before } => before.as_ref().and_then(|b| ids.iter().position(|p| *p == b)).unwrap_or(ids.len()),
                    Placement::Split(_) => ids.iter().position(|p| *p == anchor).map_or(ids.len(), |i| i + 1),
                };
                match group {
                    Node::Tabs { panels, active } => {
                        let index = index_in(&panels.iter().collect::<Vec<_>>());
                        panels.insert(index, entry.panel);
                        *active = index;
                    }
                    Node::Stack { entries } => {
                        let index = index_in(&entries.iter().map(|e| &e.panel).collect::<Vec<_>>());
                        entries.insert(index, entry);
                    }
                    Node::Split { .. } => return Err(DockError::MissingTarget),
                }
                Ok(())
            }
        }
    }
}

fn check_permissions<P>(action: &Action<P>, permissions: &impl Fn(&P) -> Permissions) -> Result<(), DockError> {
    let allowed = match action {
        Action::Move { panel, anchor, placement } | Action::OpenAt { panel, anchor, placement } => {
            let joins = matches!(placement, Placement::Tab { .. } | Placement::Split(Zone::Center));
            permissions(panel).movable && !(joins && !permissions(anchor).accepts_tabs)
        }
        Action::Float { panel, .. } => {
            let p = permissions(panel);
            p.floatable && p.movable
        }
        Action::Close { panel } => permissions(panel).closable,
        Action::MoveFloating { panel, .. } => permissions(panel).movable,
        Action::Open { anchor: Some(anchor), .. } => permissions(anchor).accepts_tabs,
        Action::Open { anchor: None, .. }
        | Action::Activate { .. }
        | Action::ResizeSplit { .. }
        | Action::SetStackOpen { .. }
        | Action::ResizeStack { .. } => true,
    };
    if allowed { Ok(()) } else { Err(DockError::NotAllowed) }
}

/// The selection after removing `removed` from a group: same identity if it survives, else the
/// removed slot clamped to the new end.
fn active_after_removal(active: usize, removed: usize, len: usize) -> usize {
    if removed < active {
        active - 1
    } else if removed == active {
        removed.min(len.saturating_sub(1))
    } else {
        active
    }
}

fn panel_node<P>(panel: P) -> Node<P> {
    Node::Tabs { panels: vec![panel], active: 0 }
}

/// Replace `target` with a split holding `new` on the `zone` side and the old node on the other.
fn wrap<P>(target: &mut Node<P>, new: Node<P>, zone: Zone, size: SplitSize) {
    let old = std::mem::replace(target, Node::Tabs { panels: Vec::new(), active: 0 });
    let axis = match zone {
        Zone::Top | Zone::Bottom => SplitAxis::Vertical,
        _ => SplitAxis::Horizontal,
    };
    let (first, second) = match zone {
        Zone::Left | Zone::Top => (new, old),
        _ => (old, new),
    };
    *target = Node::Split { axis, size, first: Box::new(first), second: Box::new(second) };
}

/// Rebuild `node` without `panel` (taken once, into `found`), dropping empty groups and
/// collapsing splits that lost a child. Recursion depth is bounded by a validated layout.
fn take_from<P: Clone + Eq>(node: Node<P>, panel: &P, found: &mut Option<StackEntry<P>>) -> Option<Node<P>> {
    match node {
        Node::Split { axis, size, first, second } => {
            let first = take_from(*first, panel, found);
            let second = take_from(*second, panel, found);
            match (first, second) {
                (Some(a), Some(b)) => Some(Node::Split { axis, size, first: Box::new(a), second: Box::new(b) }),
                (Some(only), None) | (None, Some(only)) => Some(only),
                (None, None) => None,
            }
        }
        Node::Tabs { mut panels, mut active } => {
            if found.is_none()
                && let Some(index) = panels.iter().position(|p| p == panel)
            {
                let removed = panels.remove(index);
                active = active_after_removal(active, index, panels.len());
                *found = Some(StackEntry { panel: removed, open: true, height: None });
            }
            (!panels.is_empty()).then_some(Node::Tabs { panels, active })
        }
        Node::Stack { mut entries } => {
            if found.is_none()
                && let Some(index) = entries.iter().position(|e| &e.panel == panel)
            {
                *found = Some(entries.remove(index));
            }
            (!entries.is_empty()).then_some(Node::Stack { entries })
        }
    }
}

fn stack_entry_mut<'a, P: Eq>(node: &'a mut Node<P>, panel: &P) -> Option<&'a mut StackEntry<P>> {
    match node {
        Node::Split { first, second, .. } => match stack_entry_mut(first, panel) {
            Some(e) => Some(e),
            None => stack_entry_mut(second, panel),
        },
        Node::Stack { entries } => entries.iter_mut().find(|e| &e.panel == panel),
        Node::Tabs { .. } => None,
    }
}

/// Turn every tab group of exactly `[panel]` into a one-entry accordion.
fn tabs_to_stack<P: Clone + Eq>(node: &mut Node<P>, panel: &P) {
    match node {
        Node::Split { first, second, .. } => {
            tabs_to_stack(first, panel);
            tabs_to_stack(second, panel);
        }
        Node::Tabs { panels, .. } if panels.len() == 1 && panels.first() == Some(panel) => {
            *node = Node::Stack { entries: vec![StackEntry { panel: panel.clone(), open: true, height: None }] };
        }
        _ => {}
    }
}

/// Set `size` on the first split (pre-order) with a direct child that is a tab group of exactly
/// `[panel]`.
fn set_size_beside<P: Eq>(node: &mut Node<P>, panel: &P, size: SplitSize) -> bool {
    let Node::Split { size: s, first, second, .. } = node else { return false };
    let alone = |n: &Node<P>| matches!(n, Node::Tabs { panels, .. } if panels.len() == 1 && panels.first() == Some(panel));
    if alone(first) || alone(second) {
        *s = size;
        return true;
    }
    set_size_beside(first, panel, size) || set_size_beside(second, panel, size)
}

/// The path of the first subtree (pre-order) whose panels are exactly `wanted`.
fn subtree_with<P: Eq + Hash>(root: &Node<P>, wanted: &HashSet<&P>) -> Option<Vec<bool>> {
    let mut stack: Vec<(&Node<P>, Vec<bool>)> = vec![(root, Vec::new())];
    let mut visited = 0usize;
    while let Some((node, path)) = stack.pop() {
        visited += 1;
        if visited > MAX_NODES {
            return None;
        }
        let mut ids = Vec::new();
        collect(node, &mut ids);
        if ids.len() == wanted.len() && ids.iter().all(|p| wanted.contains(p)) {
            return Some(path);
        }
        if let Node::Split { first, second, .. } = node
            && path.len() < MAX_DEPTH
        {
            let mut b = path.clone();
            b.push(true);
            stack.push((second, b));
            let mut a = path;
            a.push(false);
            stack.push((first, a));
        }
    }
    None
}
