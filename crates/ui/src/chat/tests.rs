//! Behaviour tests for the chat kit: pinning, virtualization, keys, IME, approval, AccessKit.

use egui::accesskit::{Live, Role as AkRole};
use egui::{Event, ImeEvent, Key, Modifiers, MouseWheelUnit, TouchPhase, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};

use super::*;
use crate::automation;

struct Chat {
    messages: Vec<ChatMessage>,
    revision: u64,
    drawn: usize,
    at_bottom: bool,
    visible: std::ops::Range<usize>,
}

fn msg(id: u64, text: &str) -> ChatMessage {
    let role = if id.is_multiple_of(2) { ChatRole::User } else { ChatRole::Assistant };
    ChatMessage { id, role, text: text.into(), streaming: false, revision: 0 }
}

fn chat(n: u64) -> Harness<'static, Chat> {
    let messages = (0..n).map(|i| msg(i, &format!("Message **{i}**.\n\nA second paragraph with some words."))).collect();
    let state = Chat { messages, revision: 1, drawn: 0, at_bottom: false, visible: 0..0 };
    let mut h = Harness::builder().with_size(vec2(500.0, 400.0)).build_ui_state(
        |ui, s: &mut Chat| {
            let mut drawn = 0;
            let out = Transcript::new("chat", &s.messages, s.revision).show(ui, |ui, m| {
                drawn += 1;
                let id = format!("chat.msg.{}", m.id);
                Message::new(&id, m.role, &m.text).streaming(m.streaming).show(ui);
            });
            s.drawn = drawn;
            s.at_bottom = out.at_bottom;
            s.visible = out.visible;
        },
        state,
    );
    automation::install(&h.ctx);
    h.run_steps(4);
    h
}

fn wheel(h: &Harness<'_, Chat>, dy: f32) {
    h.hover_at(egui::pos2(250.0, 200.0));
    h.event(Event::MouseWheel { unit: MouseWheelUnit::Point, delta: vec2(0.0, dy), phase: TouchPhase::Move, modifiers: Modifiers::NONE });
}

fn stream(h: &mut Harness<'_, Chat>, words: &str) {
    let s = h.state_mut();
    if let Some(last) = s.messages.last_mut() {
        last.text.push_str(words);
        last.revision += 1;
        last.streaming = true;
    }
    s.revision += 1;
}

#[test]
fn starts_at_the_bottom_and_draws_only_visible_rows() {
    let h = chat(10_000);
    let s = h.state();
    assert!(s.at_bottom);
    assert_eq!(s.visible.end, 10_000);
    assert!(s.drawn < 30, "drew {}", s.drawn);
}

#[test]
fn streaming_append_keeps_the_view_pinned() {
    let mut h = chat(50);
    for k in 0..20 {
        stream(&mut h, &format!("\n\nline {k} of a reply that keeps growing as tokens arrive"));
        h.run_steps(2);
        assert!(h.state().at_bottom, "unpinned at step {k}");
    }
    assert!(automation::find(&h.ctx, "chat.jump").is_none());
    // a new message also stays in view
    h.state_mut().messages.push(msg(50, "A new message"));
    h.state_mut().revision += 1;
    h.run_steps(3);
    assert!(h.state().at_bottom);
    assert_eq!(h.state().visible.end, 51);
}

#[test]
fn scrolled_up_stays_put_and_offers_jump() {
    let mut h = chat(50);
    wheel(&h, 600.0);
    h.run_steps(3);
    assert!(!h.state().at_bottom);
    let before = h.state().visible.clone();
    for k in 0..10 {
        stream(&mut h, &format!("\n\nmore text {k}"));
        h.run_steps(2);
    }
    assert!(!h.state().at_bottom);
    assert_eq!(h.state().visible.start, before.start, "the view moved while scrolled up");
    let jump = automation::find(&h.ctx, "chat.jump").expect("jump button");
    h.hover_at(jump.center());
    h.event(Event::PointerButton { pos: jump.center(), button: egui::PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.event(Event::PointerButton { pos: jump.center(), button: egui::PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    h.run_steps(4);
    assert!(h.state().at_bottom);
    assert!(automation::find(&h.ctx, "chat.jump").is_none());
    // and it follows the stream again
    stream(&mut h, "\n\nafter the jump");
    h.run_steps(2);
    assert!(h.state().at_bottom);
}

#[test]
fn idle_transcript_asks_for_no_frames() {
    let mut h = chat(200);
    h.run();
    assert!(h.run() <= 1, "idle transcript keeps repainting");
    wheel(&h, 300.0);
    h.run();
    assert!(h.run() <= 1, "settled transcript keeps repainting");
}

#[test]
fn transcript_is_a_polite_log() {
    let h = chat(5);
    let log = h.get_by_role(AkRole::Log);
    assert_eq!(log.accesskit_node().live(), Live::Polite);
    assert_eq!(log.accesskit_node().label().as_deref(), Some("Conversation"));
}

// composer

#[derive(Default)]
struct Input {
    draft: String,
    suggestions: Vec<String>,
    sent: Vec<String>,
    picks: Vec<usize>,
    cancels: usize,
    trigger: Option<Trigger>,
}

fn composer(suggestions: &[&str]) -> Harness<'static, Input> {
    let state = Input { suggestions: suggestions.iter().map(|s| (*s).to_owned()).collect(), ..Default::default() };
    let mut h = Harness::builder().with_size(vec2(500.0, 300.0)).build_ui_state(
        |ui, s: &mut Input| {
            ui.add_space(200.0);
            let out = Composer::new("chat.input", &mut s.draft).placeholder("Message").popup(&s.suggestions).show(ui);
            s.sent.extend(out.submitted);
            s.picks.extend(out.popup_pick);
            s.cancels += usize::from(out.cancelled);
            s.trigger = out.trigger;
        },
        state,
    );
    automation::install(&h.ctx);
    h.run();
    let field = automation::find(&h.ctx, "chat.input").expect("composer");
    h.hover_at(field.center());
    h.event(Event::PointerButton { pos: field.center(), button: egui::PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.event(Event::PointerButton { pos: field.center(), button: egui::PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    h.run();
    h
}

fn type_text(h: &mut Harness<'_, Input>, text: &str) {
    h.event(Event::Text(text.into()));
    h.run();
}

#[test]
fn enter_sends_and_shift_enter_adds_a_line() {
    let mut h = composer(&[]);
    type_text(&mut h, "hello");
    h.key_press_modifiers(Modifiers::SHIFT, Key::Enter);
    h.run();
    type_text(&mut h, "world");
    assert_eq!(h.state().draft, "hello\nworld");
    assert!(h.state().sent.is_empty());
    h.key_press(Key::Enter);
    h.run();
    assert_eq!(h.state().sent, vec!["hello\nworld".to_owned()]);
    assert_eq!(h.state().draft, "");
    // a blank draft is not sent
    h.key_press(Key::Enter);
    h.run();
    assert_eq!(h.state().sent.len(), 1);
}

#[test]
fn ime_composition_never_submits() {
    let mut h = composer(&[]);
    type_text(&mut h, "ni");
    h.event(Event::Ime(ImeEvent::Preedit { text: "日".into(), active_range_chars: None }));
    h.key_press(Key::Enter);
    h.run();
    assert!(h.state().sent.is_empty(), "sent while composing");
    h.event(Event::Ime(ImeEvent::Commit("日本".into())));
    h.key_press(Key::Enter);
    h.run();
    assert!(h.state().sent.is_empty(), "the Enter that commits must not send: {:?} {:?}", h.state().sent, h.state().draft);
    h.key_press(Key::Enter);
    h.run();
    assert_eq!(h.state().sent.len(), 1);
    assert!(h.state().sent.first().is_some_and(|s| s.starts_with("ni")));
}

#[test]
fn escape_closes_popup_then_cancels() {
    let mut h = composer(&["notes.md", "readme.md"]);
    type_text(&mut h, "see @re");
    assert_eq!(h.state().trigger, Some(Trigger { symbol: '@', query: "re".into() }));
    assert!(automation::find(&h.ctx, "chat.input.popup.item.0").is_some());
    h.key_press(Key::Escape);
    h.run();
    assert!(automation::find(&h.ctx, "chat.input.popup.item.0").is_none());
    assert_eq!(h.state().cancels, 0);
    // Escape with the popup closed cancels
    h.key_press(Key::Escape);
    h.run();
    assert_eq!(h.state().cancels, 1);
    // Tab picks and keeps the focus in the field
    type_text(&mut h, " @n");
    h.key_press(Key::Tab);
    h.run();
    assert_eq!(h.state().draft, "see @re @notes.md ");
    type_text(&mut h, "x");
    assert_eq!(h.state().draft, "see @re @notes.md x");
}

#[test]
fn popup_picks_with_keys_and_pointer() {
    let mut h = composer(&["notes.md", "readme.md"]);
    type_text(&mut h, "see @re");
    h.key_press(Key::ArrowDown);
    h.run();
    assert_eq!(automation::find(&h.ctx, "chat.input.popup.item.1").and_then(|e| e.selected), Some(true));
    h.key_press(Key::Enter);
    h.run();
    assert_eq!(h.state().picks, vec![1]);
    assert_eq!(h.state().draft, "see @readme.md ");
    assert!(h.state().sent.is_empty());
    type_text(&mut h, "and @");
    let item = automation::find(&h.ctx, "chat.input.popup.item.0").expect("popup");
    h.hover_at(item.center());
    h.event(Event::PointerButton { pos: item.center(), button: egui::PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.event(Event::PointerButton { pos: item.center(), button: egui::PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    h.run();
    assert_eq!(h.state().draft, "see @readme.md and @notes.md ");
    // typing goes on in the field
    type_text(&mut h, "!");
    assert_eq!(h.state().draft, "see @readme.md and @notes.md !");
}

#[test]
fn slash_only_at_line_start() {
    assert_eq!(composer::trigger_for_test("/he", 3), Some(('/', "he".into(), 0)));
    assert_eq!(composer::trigger_for_test("a /he", 5), None);
    assert_eq!(composer::trigger_for_test("a\n/he", 5), Some(('/', "he".into(), 2)));
    assert_eq!(composer::trigger_for_test("mail me@x", 9), None);
    assert_eq!(composer::trigger_for_test("é @x", 5), Some(('@', "x".into(), 3)));
    assert_eq!(composer::trigger_for_test("x", 99), None);
}

// tool card

#[derive(Default)]
struct Card {
    pending: bool,
    answers: Vec<Approval>,
}

fn card() -> Harness<'static, Card> {
    let mut h = Harness::builder().with_size(vec2(500.0, 300.0)).build_ui_state(
        |ui, s: &mut Card| {
            let out = ToolCard::new("tool.1", "Read file", if s.pending { ToolStatus::NeedsApproval } else { ToolStatus::Succeeded })
                .details(|ui| {
                    ui.label("path: notes.md");
                })
                .approval(&mut s.pending)
                .show(ui);
            s.answers.extend(out.approval);
        },
        Card { pending: true, answers: vec![] },
    );
    automation::install(&h.ctx);
    h.run();
    h
}

#[test]
fn approval_by_keyboard() {
    let mut h = card();
    // Tab: toggle, approve, deny
    for _ in 0..2 {
        h.key_press(Key::Tab);
        h.run();
    }
    h.key_press(Key::Enter);
    h.run();
    assert_eq!(h.state().answers, vec![Approval::Approved]);
    assert!(!h.state().pending);
    assert!(automation::find(&h.ctx, "tool.1.approve").is_none());
    assert_eq!(automation::find(&h.ctx, "tool.1").map(|e| e.value), Some("Done".to_owned()));
}

#[test]
fn approval_by_automation() {
    let mut h = card();
    let deny = automation::find(&h.ctx, "tool.1.deny").expect("deny");
    h.hover_at(deny.center());
    h.event(Event::PointerButton { pos: deny.center(), button: egui::PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.event(Event::PointerButton { pos: deny.center(), button: egui::PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    h.run();
    assert_eq!(h.state().answers, vec![Approval::Denied]);
}

#[test]
fn details_fold() {
    let mut h = card();
    assert!(h.query_by_label("path: notes.md").is_none());
    let toggle = automation::find(&h.ctx, "tool.1.toggle").expect("toggle");
    h.hover_at(toggle.center());
    h.event(Event::PointerButton { pos: toggle.center(), button: egui::PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.event(Event::PointerButton { pos: toggle.center(), button: egui::PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    h.run();
    assert!(h.query_by_label("path: notes.md").is_some());
}
