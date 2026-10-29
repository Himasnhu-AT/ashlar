//! Tree view: an expandable hierarchy shown as a virtualized list.
//!
//! The app's data implements [`TreeModel`] (node ids are `u64`); [`TreeState`] (app-owned,
//! serializable) holds which nodes are expanded and which is selected. The visible rows are
//! flattened only when the model's revision or the expansion changes, so a steady frame costs
//! O(visible rows) even for a large tree.
//!
//! Keyboard (when the tree has focus): Up/Down move, Right expands or enters the first child,
//! Left collapses or goes to the parent, Enter activates, Home/End/Page Up/Page Down jump.

use std::collections::HashSet;

use egui::{Rect, Sense, Stroke, Ui, pos2, text::LayoutJob, vec2};

use crate::automation::{self, Details, Role};
use crate::cache;
use crate::tokens::{self, Tokens};
use crate::virtual_list::{Kind, RowHeights, VirtualList, show_rows};

/// Deepest level shown (deeper nodes are not listed; cycles in a model end here too).
pub const MAX_DEPTH: usize = 128;
/// Most rows flattened.
pub const MAX_ROWS: usize = 2_000_000;

/// The app's hierarchy.
pub trait TreeModel {
    /// Children of `parent` (`None`: the roots), appended to `out` in display order.
    fn children(&self, parent: Option<u64>, out: &mut Vec<u64>);
    /// Whether `node` has children (shows the disclosure arrow without listing them).
    fn has_children(&self, node: u64) -> bool;
    /// The node's label.
    fn label(&self, node: u64) -> &str;
}

#[derive(Clone, Copy, Debug)]
struct FlatRow {
    id: u64,
    parent: Option<u64>,
    depth: u16,
    has_children: bool,
}

/// Expansion and selection of one tree (keep it in the app's view state).
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct TreeState {
    expanded: HashSet<u64>,
    selected: Option<u64>,
    #[serde(skip)]
    flat: Vec<FlatRow>,
    #[serde(skip)]
    flat_key: Option<(u64, u64)>,
    #[serde(skip)]
    generation: u64,
    #[serde(skip)]
    selected_row: Option<usize>,
}

impl TreeState {
    pub fn is_expanded(&self, node: u64) -> bool {
        self.expanded.contains(&node)
    }
    pub fn set_expanded(&mut self, node: u64, expanded: bool) {
        let changed = if expanded { self.expanded.insert(node) } else { self.expanded.remove(&node) };
        if changed {
            self.generation = self.generation.wrapping_add(1);
        }
    }
    pub fn selected(&self) -> Option<u64> {
        self.selected
    }
    pub fn select(&mut self, node: Option<u64>) {
        self.selected = node;
        self.selected_row = None;
    }
    /// Rows currently flattened (visible when scrolled to).
    pub fn row_count(&self) -> usize {
        self.flat.len()
    }

    fn flatten(&mut self, model: &dyn TreeModel, revision: u64) {
        let key = (revision, self.generation);
        if self.flat_key == Some(key) {
            return;
        }
        self.flat.clear();
        // iterative DFS: a stack of (children, next index, depth, parent)
        let mut roots = Vec::new();
        model.children(None, &mut roots);
        let mut stack: Vec<(Vec<u64>, usize, u16, Option<u64>)> = vec![(roots, 0, 0, None)];
        while let Some((nodes, next, depth, parent)) = stack.last_mut() {
            let Some(&id) = nodes.get(*next) else {
                stack.pop();
                continue;
            };
            *next += 1;
            let (depth, parent) = (*depth, *parent);
            if self.flat.len() >= MAX_ROWS {
                break;
            }
            let has_children = model.has_children(id);
            self.flat.push(FlatRow { id, parent, depth, has_children });
            if has_children && self.expanded.contains(&id) && (depth as usize) + 1 < MAX_DEPTH {
                let mut kids = Vec::new();
                model.children(Some(id), &mut kids);
                stack.push((kids, 0, depth + 1, Some(id)));
            }
        }
        self.flat_key = Some(key);
        self.selected_row = None;
    }

    fn selected_row(&mut self) -> Option<usize> {
        let sel = self.selected?;
        if let Some(r) = self.selected_row.filter(|r| self.flat.get(*r).is_some_and(|f| f.id == sel)) {
            return Some(r);
        }
        self.selected_row = self.flat.iter().position(|f| f.id == sel);
        self.selected_row
    }
}

/// What the tree reports.
pub struct TreeResponse {
    pub response: egui::Response,
    /// The node the user selected (already stored in the state).
    pub selected: Option<u64>,
    /// Double click or Enter.
    pub activated: Option<u64>,
    /// A node expanded (`true`) or collapsed (already applied to the state).
    pub toggled: Option<(u64, bool)>,
    pub secondary_clicked: Option<u64>,
}

/// The view. See the [module docs](self).
pub struct TreeView<'a> {
    id: &'a str,
    state: &'a mut TreeState,
    model: &'a dyn TreeModel,
    revision: u64,
    max_height: Option<f32>,
}

impl<'a> TreeView<'a> {
    /// `revision` must change whenever the model's structure or labels change.
    pub fn new(id: &'a str, state: &'a mut TreeState, model: &'a dyn TreeModel, revision: u64) -> Self {
        TreeView { id, state, model, revision, max_height: None }
    }
    pub fn max_height(mut self, h: f32) -> Self {
        self.max_height = Some(h);
        self
    }

    pub fn show(self, ui: &mut Ui) -> TreeResponse {
        let t = tokens::current(ui.ctx());
        let state = self.state;
        state.flatten(self.model, self.revision);
        let row_h = t.control_height - 2.0;
        let indent = t.space.xl;
        let base = ui.make_persistent_id(self.id);
        let cursor = state.selected_row();
        let mut list = VirtualList::new(self.id, state.flat.len(), RowHeights::Uniform(row_h)).selected(cursor);
        if let Some(h) = self.max_height {
            list = list.max_height(h);
        }
        let mut toggled = None;
        let flat = &state.flat;
        let model = self.model;
        let revision = self.revision;
        let tree_id = self.id;
        let expanded = &state.expanded;
        let out = show_rows(ui, base, &list, Kind { role: Role::Tree, rows: false }, None, |ui, row| {
            let Some(node) = flat.get(row.index) else { return };
            let x0 = row.rect.left() + node.depth as f32 * indent;
            let arrow = Rect::from_min_size(pos2(x0, row.rect.top()), vec2(indent, row.rect.height()));
            let is_open = expanded.contains(&node.id);
            if node.has_children {
                let resp = ui.interact(arrow, base.with(("toggle", node.id)), Sense::click());
                if resp.clicked() {
                    toggled = Some((node.id, !is_open));
                }
                let color = if resp.hovered() { t.text.primary } else { t.text.secondary };
                disclosure(ui, arrow.center(), is_open, color);
            }
            let label = model.label(node.id);
            let color = if row.selected { t.selection_text } else { t.text.primary };
            let font = Tokens::font(t.type_scale.body);
            let galley = cache::galley(ui, ("ashlar_ui::tree", base, node.id), cache::hash_of(&(revision, color, font.size.to_bits())), || {
                LayoutJob::simple_singleline(label.to_owned(), font, color)
            });
            let text_pos = pos2(arrow.right() + 2.0, row.rect.center().y - galley.size().y / 2.0);
            ui.painter().galley(text_pos, galley, color);
            let id = node.id;
            automation::register_fmt(
                ui,
                row.rect,
                format_args!("{tree_id}.node.{id}"),
                label,
                Role::TreeItem,
                true,
                Details { value: node.has_children.then_some(if is_open { "expanded" } else { "collapsed" }), selected: Some(row.selected) },
            );
        });

        // keyboard: Left/Right
        let mut selected = out.select.and_then(|i| state.flat.get(i)).map(|f| f.id);
        if crate::edit::has_keyboard(&out.response)
            && let Some(cur) = cursor.and_then(|i| state.flat.get(i)).copied()
        {
            let (left, right) = ui.input(|i| (i.key_pressed(egui::Key::ArrowLeft), i.key_pressed(egui::Key::ArrowRight)));
            let open = state.expanded.contains(&cur.id);
            if right && cur.has_children {
                if open {
                    selected = cursor.and_then(|i| state.flat.get(i + 1)).filter(|n| n.parent == Some(cur.id)).map(|n| n.id).or(selected);
                } else {
                    toggled = Some((cur.id, true));
                }
            } else if left {
                if cur.has_children && open {
                    toggled = Some((cur.id, false));
                } else if cur.parent.is_some() {
                    selected = cur.parent;
                }
            }
        }
        if let Some((node, open)) = toggled {
            state.set_expanded(node, open);
        }
        if let Some(node) = selected {
            state.select(Some(node));
        }
        let activated = out.activated.and_then(|i| state.flat.get(i)).map(|f| f.id);
        let secondary_clicked = out.secondary_clicked.and_then(|i| state.flat.get(i)).map(|f| f.id);
        TreeResponse { response: out.response, selected, activated, toggled, secondary_clicked }
    }
}

fn disclosure(ui: &Ui, c: egui::Pos2, open: bool, color: egui::Color32) {
    let s = 3.5;
    let stroke = Stroke::new(1.5, color);
    if open {
        ui.painter().line_segment([pos2(c.x - s, c.y - s * 0.5), pos2(c.x, c.y + s * 0.5)], stroke);
        ui.painter().line_segment([pos2(c.x, c.y + s * 0.5), pos2(c.x + s, c.y - s * 0.5)], stroke);
    } else {
        ui.painter().line_segment([pos2(c.x - s * 0.5, c.y - s), pos2(c.x + s * 0.5, c.y)], stroke);
        ui.painter().line_segment([pos2(c.x + s * 0.5, c.y), pos2(c.x - s * 0.5, c.y + s)], stroke);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::click;
    use egui_kittest::Harness;

    /// Roots 0..roots; node n has children n*10+1 ..= n*10+fanout while below `depth` levels.
    struct Numbers {
        roots: u64,
        fanout: u64,
        labels: Vec<String>,
    }

    impl Numbers {
        fn new(roots: u64, fanout: u64) -> Self {
            Numbers { roots, fanout, labels: (0..1000).map(|i| format!("node {i}")).collect() }
        }
    }

    impl TreeModel for Numbers {
        fn children(&self, parent: Option<u64>, out: &mut Vec<u64>) {
            match parent {
                None => out.extend(1..=self.roots),
                Some(p) if p < 100 => out.extend((1..=self.fanout).map(|k| p * 10 + k)),
                Some(_) => {}
            }
        }
        fn has_children(&self, node: u64) -> bool {
            node < 100
        }
        fn label(&self, node: u64) -> &str {
            self.labels.get(node as usize).map_or("node", String::as_str)
        }
    }

    struct Cyclic;
    impl TreeModel for Cyclic {
        fn children(&self, _: Option<u64>, out: &mut Vec<u64>) {
            out.push(1);
        }
        fn has_children(&self, _: u64) -> bool {
            true
        }
        fn label(&self, _: u64) -> &str {
            "loop"
        }
    }

    #[test]
    fn flattening_follows_expansion_and_is_cached() {
        let model = Numbers::new(3, 2);
        let mut s = TreeState::default();
        s.flatten(&model, 1);
        assert_eq!(s.flat.iter().map(|f| f.id).collect::<Vec<_>>(), [1, 2, 3]);
        s.set_expanded(2, true);
        s.flatten(&model, 1);
        assert_eq!(s.flat.iter().map(|f| (f.id, f.depth)).collect::<Vec<_>>(), [(1, 0), (2, 0), (21, 1), (22, 1), (3, 0)]);
        s.set_expanded(21, true);
        s.flatten(&model, 1);
        assert_eq!(s.flat.iter().map(|f| f.id).collect::<Vec<_>>(), [1, 2, 21, 211, 212, 22, 3]);
        assert_eq!(s.flat[3].parent, Some(21));
        // a cyclic model stops at MAX_DEPTH
        let mut c = TreeState::default();
        c.set_expanded(1, true);
        c.flatten(&Cyclic, 0);
        assert_eq!(c.flat.len(), MAX_DEPTH);
        let json = serde_json::to_string(&s).unwrap();
        let back: TreeState = serde_json::from_str(&json).unwrap();
        assert!(back.is_expanded(21));
    }

    #[test]
    fn click_toggle_and_keyboard_navigation() {
        struct S {
            state: TreeState,
            model: Numbers,
            activated: Option<u64>,
        }
        let mut h = Harness::builder().with_size(vec2(400.0, 400.0)).build_ui_state(
            |ui, s: &mut S| {
                let out = TreeView::new("outline", &mut s.state, &s.model, 1).show(ui);
                s.activated = s.activated.or(out.activated);
            },
            S { state: TreeState::default(), model: Numbers::new(3, 3), activated: None },
        );
        crate::automation::install(&h.ctx);
        h.step();
        h.step();
        let n2 = crate::automation::find(&h.ctx, "outline.node.2").unwrap();
        assert_eq!(n2.value, "collapsed");
        assert_eq!(n2.label, "node 2");
        // click the arrow area (left edge of the row)
        click(&mut h, egui::pos2(n2.egui_rect().left() + 12.0, n2.center().y));
        assert!(h.state().state.is_expanded(2));
        h.step();
        assert!(crate::automation::find(&h.ctx, "outline.node.21").is_some());
        // select node 2 and walk with the keyboard
        click(&mut h, n2.center() + vec2(60.0, 0.0));
        assert_eq!(h.state().state.selected(), Some(2));
        h.key_press(egui::Key::ArrowRight); // open: go to first child
        h.step();
        assert_eq!(h.state().state.selected(), Some(21));
        h.key_press(egui::Key::ArrowRight); // closed: expand
        h.step();
        assert!(h.state().state.is_expanded(21));
        h.key_press(egui::Key::ArrowLeft); // open: collapse
        h.step();
        assert!(!h.state().state.is_expanded(21));
        h.key_press(egui::Key::ArrowLeft); // closed: to parent
        h.step();
        assert_eq!(h.state().state.selected(), Some(2));
        h.key_press(egui::Key::ArrowDown);
        h.step();
        assert_eq!(h.state().state.selected(), Some(21));
        h.key_press(egui::Key::Enter);
        h.step();
        assert_eq!(h.state().activated, Some(21));
    }

    #[test]
    fn large_expanded_tree_paints_only_visible_rows() {
        struct Wide;
        impl TreeModel for Wide {
            fn children(&self, parent: Option<u64>, out: &mut Vec<u64>) {
                match parent {
                    None => out.extend(0..100),
                    Some(p) if p < 100 => out.extend((0..1000).map(|k| 1000 + p * 1000 + k)),
                    Some(_) => {}
                }
            }
            fn has_children(&self, node: u64) -> bool {
                node < 100
            }
            fn label(&self, _: u64) -> &str {
                "leaf"
            }
        }
        let mut state = TreeState::default();
        for r in 0..100 {
            state.set_expanded(r, true);
        }
        let ctx = egui::Context::default();
        crate::automation::install(&ctx);
        for _ in 0..3 {
            crate::test_support::run(
                &ctx,
                egui::RawInput { screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(400.0, 400.0))), ..Default::default() },
                |ui| {
                    TreeView::new("big", &mut state, &Wide, 7).show(ui);
                },
            );
        }
        assert_eq!(state.row_count(), 100_100);
        let rows = crate::automation::query(&ctx, "big.node.").len();
        assert!(crate::automation::query(&ctx, "big.row.").is_empty());
        assert_eq!(crate::automation::find(&ctx, "big").map(|e| e.role), Some(Role::Tree));
        assert!(rows > 0 && rows < 40, "{rows} rows registered");
    }
}
