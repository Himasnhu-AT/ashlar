#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `VirtualList::scroll_to` lands its row fully in view: from the top, the middle and the end,
//! back up to the top, without moving a row that is already in view, and after a fling.

use std::cell::Cell;
use std::rc::Rc;

use ashlar_ui::automation;
use ashlar_ui::virtual_list::{RowHeights, VirtualList};
use egui::{Event, Modifiers, PointerButton, Rect, pos2, vec2};

const DT: f64 = 1.0 / 120.0;
const ROW_H: f32 = 20.0;
const ROWS: usize = 1000;

struct Rig {
    ctx: egui::Context,
    n: usize,
    target: Rc<Cell<Option<usize>>>,
    fling: bool,
}

impl Rig {
    fn new(fling: bool) -> Self {
        let ctx = egui::Context::default();
        automation::install(&ctx);
        let mut rig = Rig { ctx, n: 0, target: Rc::new(Cell::new(None)), fling };
        rig.settle(2);
        rig
    }

    fn frame(&mut self, events: Vec<Event>) {
        let input = egui::RawInput {
            time: Some(self.n as f64 * DT),
            predicted_dt: DT as f32,
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(600.0, 400.0))),
            events,
            ..Default::default()
        };
        // the request lasts one frame, as an app passes it on the frame it wants the jump
        let target = self.target.take();
        let fling = self.fling;
        let mut out = self.ctx.run_ui(input, |ui| {
            ui.set_max_height(300.0);
            VirtualList::new("t.list", ROWS, RowHeights::Uniform(ROW_H))
                .max_height(300.0)
                .row_label(&|_| "row")
                .fling(fling)
                .scroll_to(target)
                .show(ui, |_, _| {});
        });
        out.textures_delta.clear();
        self.n += 1;
    }

    fn settle(&mut self, frames: usize) {
        for _ in 0..frames {
            self.frame(Vec::new());
        }
    }

    fn scroll_to(&mut self, row: usize) {
        self.target.set(Some(row));
        self.settle(3);
    }

    fn list(&self) -> Rect {
        automation::find(&self.ctx, "t.list").expect("list registered").egui_rect()
    }

    /// The list's scroll offset, from the first registered row.
    fn offset(&self) -> f32 {
        let top = self.list().top();
        let rows = automation::query(&self.ctx, "t.list.row.");
        let (i, el) = rows
            .iter()
            .filter_map(|e| e.id.rsplit('.').next().and_then(|n| n.parse::<usize>().ok()).map(|i| (i, e)))
            .min_by_key(|(i, _)| *i)
            .expect("rows");
        i as f32 * ROW_H - (el.rect[1] - top)
    }

    fn assert_in_view(&self, row: usize, when: &str) {
        let list = self.list();
        let r = automation::find(&self.ctx, &format!("t.list.row.{row}")).unwrap_or_else(|| panic!("{when}: row {row} not shown")).egui_rect();
        assert!(r.top() >= list.top() - 0.5 && r.bottom() <= list.bottom() + 0.5, "{when}: row {row} at {r:?}, list {list:?}");
    }

    fn fling_up(&mut self) {
        let list = self.list();
        let press = |pos, pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
        let mut at = pos2(list.center().x, list.bottom() - 20.0);
        self.frame(vec![Event::PointerMoved(at)]);
        self.frame(vec![press(at, true)]);
        for _ in 0..20 {
            at.y -= 6.0;
            self.frame(vec![Event::PointerMoved(at)]);
        }
        at.y -= 6.0;
        self.frame(vec![Event::PointerMoved(at), press(at, false)]);
    }
}

fn check(fling: bool) {
    let mut rig = Rig::new(fling);
    rig.scroll_to(500);
    rig.assert_in_view(500, "middle");
    // a row already in view stays where it is
    let before = rig.offset();
    rig.scroll_to(495);
    rig.assert_in_view(495, "already in view");
    assert!((rig.offset() - before).abs() < 0.5, "a visible row moved the list: {before} → {}", rig.offset());
    rig.scroll_to(0);
    rig.assert_in_view(0, "back to the start");
    rig.scroll_to(ROWS - 1);
    rig.assert_in_view(ROWS - 1, "end");
    rig.scroll_to(250);
    rig.assert_in_view(250, "up from the end");
}

#[test]
fn scroll_to_shows_the_row() {
    check(false);
}

#[test]
fn scroll_to_shows_the_row_in_a_fling_list() {
    check(true);
}

#[test]
fn scroll_to_after_a_fling_shows_the_row() {
    let mut rig = Rig::new(true);
    rig.fling_up();
    rig.settle(10);
    // mid-fling: the jump wins over the glide
    rig.scroll_to(0);
    rig.assert_in_view(0, "during a fling");
    rig.fling_up();
    rig.settle(300);
    assert!(rig.offset() > 100.0, "the fling moved the list: {}", rig.offset());
    rig.scroll_to(3);
    rig.assert_in_view(3, "after a fling");
    rig.scroll_to(700);
    rig.assert_in_view(700, "down after a fling");
}
