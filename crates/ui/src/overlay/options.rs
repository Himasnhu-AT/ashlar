//! The virtualized option list shared by the select and the combobox.

use egui::accesskit::Role as NodeRole;
use egui::{Align2, Stroke, Ui, pos2, vec2};

use super::VISIBLE_OPTIONS;
use crate::automation::{self, Details, Role};
use crate::tokens::{self, Tokens};
use crate::virtual_list::{self, Kind, RowHeights, VirtualList};

const LISTBOX: Kind = Kind { role: Role::ListBox, rows: false, node: NodeRole::ListBox, item: NodeRole::ListBoxOption };

pub(crate) struct OptionList<'a> {
    /// Automation id of the select or combobox; the list is `<id>.list`.
    pub id: &'a str,
    pub rows: usize,
    /// The option index shown in each row.
    pub option_of: &'a dyn Fn(usize) -> Option<usize>,
    /// The label shown in each row.
    pub label: &'a dyn Fn(usize) -> &'a str,
    /// The keyboard cursor (a row).
    pub cursor: Option<usize>,
    /// The chosen option (an option index), marked with a check.
    pub chosen: Option<usize>,
    pub scroll_to: Option<usize>,
}

impl OptionList<'_> {
    /// Returns the clicked row.
    pub fn show(self, ui: &mut Ui) -> Option<usize> {
        let t = tokens::current(ui.ctx());
        let row_h = t.control_height;
        let list_id = format!("{}.list", self.id);
        let label_fn = self.label;
        let label = |i: usize| -> &str { label_fn(i) };
        let mut list = VirtualList::new(&list_id, self.rows, RowHeights::Uniform(row_h))
            .selected(self.cursor)
            .scroll_to(self.scroll_to)
            .max_height(row_h * self.rows.clamp(1, VISIBLE_OPTIONS) as f32)
            .row_label(&label);
        let base = ui.make_persistent_id(list_id.as_str());
        let id = self.id;
        let font = Tokens::font(t.type_scale.body);
        let out = virtual_list::show_rows(ui, base, &mut list, LISTBOX, None, |ui, row| {
            let Some(option) = (self.option_of)(row.index) else { return };
            let label = (self.label)(row.index);
            let chosen = self.chosen == Some(option);
            let color = if row.selected { t.selection_text } else { t.text.primary };
            let gutter = row_h * 0.7;
            if chosen {
                let c = pos2(row.rect.left() + gutter / 2.0, row.rect.center().y);
                let s = Stroke::new(1.6, color);
                ui.painter().line_segment([c + vec2(-4.0, 0.0), c + vec2(-1.0, 3.0)], s);
                ui.painter().line_segment([c + vec2(-1.0, 3.0), c + vec2(4.5, -3.5)], s);
            }
            ui.painter().text(pos2(row.rect.left() + gutter, row.rect.center().y), Align2::LEFT_CENTER, label, font.clone(), color);
            automation::register_fmt(
                ui,
                row.rect,
                format_args!("{id}.option.{option}"),
                label,
                Role::ListOption,
                true,
                Details { value: None, selected: Some(chosen) },
            );
        });
        out.clicked
    }
}
