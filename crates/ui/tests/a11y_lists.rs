#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Screen-reader view of virtual lists, tables and trees, and list selection and type-ahead.

use ashlar_ui::selection::Selection;
use ashlar_ui::tree::{TreeModel, TreeState, TreeView};
use ashlar_ui::virtual_list::{Column, RowHeights, VirtualList, VirtualTable};
use egui::accesskit::Role;
use egui::{Event, Key, Modifiers};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};

struct ListState {
    labels: Vec<String>,
    sel: Selection,
}

fn list_harness(labels: Vec<String>) -> Harness<'static, ListState> {
    Harness::builder().with_size((300.0, 400.0)).build_ui_state(
        |ui, s: &mut ListState| {
            let labels = &s.labels;
            let label = |i: usize| labels.get(i).map_or("", String::as_str);
            VirtualList::new("files", labels.len(), RowHeights::Uniform(20.0)).row_label(&label).type_ahead(&label).selection(&mut s.sel).show(
                ui,
                |ui, row| {
                    ui.label(label(row.index));
                },
            );
        },
        ListState { labels, sel: Selection::new() },
    )
}

fn numbered(n: usize) -> Vec<String> {
    (0..n).map(|i| format!("row {i}")).collect()
}

#[test]
fn visible_rows_only_are_list_items_with_position_and_name() {
    let mut h = list_harness(numbered(100_000));
    h.run();
    let items = h.query_all_by_role(Role::ListItem).count();
    assert!(items > 10 && items < 60, "{items} row nodes");
    let list = h.get_by_role(Role::List);
    // the automation id is not a name a screen reader should read out
    assert_eq!(list.accesskit_node().label().as_deref(), None);
    assert!(list.accesskit_node().is_multiselectable());
    let row = h.get_by_role_and_label(Role::ListItem, "row 3");
    let node = row.accesskit_node();
    assert_eq!((node.position_in_set(), node.size_of_set()), (Some(4), Some(100_000)));
    assert_eq!(node.is_selected(), Some(false));
    let mut up = node.parent();
    let mut chain = Vec::new();
    while let Some(p) = up {
        chain.push(p.role());
        up = p.parent();
    }
    assert!(chain.contains(&Role::List), "row ancestors: {chain:?}");
}

#[test]
fn click_shift_down_extends_and_command_a_selects_all() {
    let mut h = list_harness(numbered(1000));
    h.run();
    h.get_by_role_and_label(Role::ListItem, "row 2").click();
    h.run();
    assert_eq!(h.state().sel.iter().collect::<Vec<_>>(), vec![2]);
    h.key_press_modifiers(Modifiers::SHIFT, Key::ArrowDown);
    h.run();
    h.key_press_modifiers(Modifiers::SHIFT, Key::ArrowDown);
    h.run();
    assert_eq!(h.state().sel.iter().collect::<Vec<_>>(), vec![2, 3, 4]);
    assert_eq!((h.state().sel.anchor(), h.state().sel.cursor()), (Some(2), Some(4)));
    assert_eq!(h.get_by_role_and_label(Role::ListItem, "row 4").accesskit_node().is_selected(), Some(true));

    h.get_by_role_and_label(Role::ListItem, "row 7").click_modifiers(Modifiers::COMMAND);
    h.run();
    assert_eq!(h.state().sel.iter().collect::<Vec<_>>(), vec![2, 3, 4, 7]);
    h.key_press(Key::ArrowDown);
    h.run();
    assert_eq!(h.state().sel.iter().collect::<Vec<_>>(), vec![8]);

    h.key_press_modifiers(Modifiers::COMMAND, Key::A);
    h.run();
    assert_eq!(h.state().sel.len(), 1000);
}

#[test]
fn type_ahead_jumps_to_matching_row() {
    let names = ["apple", "banana", "foo", "fig", "grape", "Fit"];
    let mut h = list_harness(names.iter().map(|s| (*s).to_owned()).collect());
    h.run();
    h.get_by_role_and_label(Role::ListItem, "apple").click();
    h.run();
    h.event(Event::Text("f".into()));
    h.run();
    assert_eq!(h.state().sel.cursor(), Some(2));
    h.event(Event::Text("i".into()));
    h.run();
    assert_eq!(h.state().sel.cursor(), Some(3));
    assert_eq!(h.state().sel.iter().collect::<Vec<_>>(), vec![3]);
    // same prefix, case-insensitive, continues past the cursor after a pause
    for _ in 0..90 {
        h.step();
    }
    h.event(Event::Text("F".into()));
    h.run();
    h.event(Event::Text("i".into()));
    h.run();
    assert_eq!(h.state().sel.cursor(), Some(5));
}

#[test]
fn table_rows_have_the_row_role() {
    let columns = [Column::new("Name", 120.0), Column::new("Size", 80.0)];
    let mut h = Harness::builder().with_size((300.0, 300.0)).build_ui(move |ui| {
        let label = |i: usize| if i == 0 { "first" } else { "other" };
        VirtualTable::new("table", &columns, 50_000, RowHeights::Uniform(20.0)).row_label(&label).show(ui, |ui, r, c| {
            ui.label(format!("{r}:{c}"));
        });
    });
    h.run();
    assert_eq!(h.get_by_role(Role::Table).accesskit_node().role(), Role::Table);
    let rows = h.query_all_by_role(Role::Row).count();
    assert!(rows > 5 && rows < 60, "{rows}");
    assert_eq!(h.get_by_role_and_label(Role::Row, "first").accesskit_node().position_in_set(), Some(1));
}

struct Outline;
impl TreeModel for Outline {
    fn children(&self, parent: Option<u64>, out: &mut Vec<u64>) {
        match parent {
            None => out.extend([1, 2]),
            Some(1) => out.extend([10, 11, 12]),
            _ => {}
        }
    }
    fn has_children(&self, node: u64) -> bool {
        node == 1 || node == 2
    }
    fn label(&self, node: u64) -> &str {
        match node {
            1 => "src",
            2 => "docs",
            10 => "main.rs",
            11 => "lib.rs",
            _ => "util.rs",
        }
    }
}

#[test]
fn tree_items_report_level_expanded_and_sibling_position() {
    let mut state = TreeState::default();
    state.set_expanded(1, true);
    state.select(Some(11));
    let mut h = Harness::builder().with_size((300.0, 300.0)).build_ui_state(
        |ui, state: &mut TreeState| {
            TreeView::new("outline", state, &Outline, 1).show(ui);
        },
        state,
    );
    h.run();
    assert_eq!(h.get_by_role(Role::Tree).accesskit_node().role(), Role::Tree);
    assert_eq!(h.query_all_by_role(Role::TreeItem).count(), 5);
    let src = h.get_by_role_and_label(Role::TreeItem, "src");
    let n = src.accesskit_node();
    assert_eq!((n.level(), n.data().is_expanded(), n.position_in_set(), n.size_of_set()), (Some(1), Some(true), Some(1), Some(2)));
    let docs = h.get_by_role_and_label(Role::TreeItem, "docs").accesskit_node();
    assert_eq!(docs.data().is_expanded(), Some(false));
    let lib = h.get_by_role_and_label(Role::TreeItem, "lib.rs").accesskit_node();
    assert_eq!((lib.level(), lib.data().is_expanded(), lib.position_in_set(), lib.size_of_set()), (Some(2), None, Some(2), Some(3)));
    assert_eq!(lib.is_selected(), Some(true));
}
