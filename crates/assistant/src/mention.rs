//! `@` mentions and `/` commands in the message box: finding the word being typed after `@` or
//! `/` ([`trigger_at`]), ranking what the picker offers ([`rank`], [`score`]), putting the
//! chosen item into the text ([`complete`]), and the mention itself ([`Mention`]), whose details
//! reach the model as text while the message shows only a chip.
//!
//! What can be mentioned is the app's: it builds [`Candidate`]s (its documents, the parts of the
//! open one, files dropped on the panel, docs pages) and keeps the chosen [`Mention`]s with the
//! draft until it is sent ([`retain_present`] drops the ones whose `@label` was deleted).
//!
//! Cursor positions are character indices (what a text editor reports), not byte offsets.

use serde::{Deserialize, Serialize};

/// Something the person pointed at: a document, a part of one, a file, a docs page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mention {
    /// What it is, in the app's words (`layer`, `file`, `doc`…).
    pub kind: String,
    /// The app's id for it (what its tools take).
    pub id: String,
    /// The name shown on the chip and written after `@` in the message.
    pub label: String,
    /// What the model is told about it (ids, names, paths, sizes), one fact per line.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
}

impl Mention {
    pub fn new(kind: impl Into<String>, id: impl Into<String>, label: impl Into<String>) -> Self {
        Self { kind: kind.into(), id: id.into(), label: label.into(), detail: String::new() }
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = detail.into();
        self
    }

    /// How the mention is written in the message (`@Main comp`).
    pub fn token(&self) -> String {
        format!("@{}", self.label)
    }

    /// The text the model sees for it.
    pub fn prompt_text(&self) -> String {
        let mut s = format!("<mention kind=\"{}\" id=\"{}\" label=\"{}\">", attr(&self.kind), attr(&self.id), attr(&self.label));
        if !self.detail.trim().is_empty() {
            s.push('\n');
            s.push_str(self.detail.trim());
            s.push('\n');
        }
        s.push_str("</mention>");
        s
    }
}

fn attr(s: &str) -> String {
    s.replace('&', "&amp;").replace('"', "&quot;").replace('<', "&lt;").replace('>', "&gt;").replace('\n', " ")
}

/// One row of the `@` picker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candidate {
    pub mention: Mention,
    /// The second line in the picker (`Layer in Main comp`, a path).
    #[serde(default)]
    pub subtitle: String,
}

impl Candidate {
    pub fn new(mention: Mention, subtitle: impl Into<String>) -> Self {
        Self { mention, subtitle: subtitle.into() }
    }
}

/// How well `query` matches `text` (higher is better), `None` when it doesn't. Case does not
/// matter. The whole text, then its start, then a word's start, then anywhere, then the
/// letters in order. An empty query matches everything equally.
pub fn score(text: &str, query: &str) -> Option<u32> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return Some(1);
    }
    let t = text.to_lowercase();
    if t == q {
        return Some(1000);
    }
    if t.starts_with(&q) {
        return Some(800);
    }
    if let Some(i) = t.find(&q) {
        let at_word = t.get(..i).and_then(|p| p.chars().last()).is_some_and(|c| !c.is_alphanumeric());
        return Some(if at_word { 600 } else { 400 });
    }
    // the letters in order, closer together is better
    let mut chars = t.chars().enumerate();
    let mut first = None;
    let mut last = 0usize;
    for qc in q.chars() {
        let (i, _) = chars.by_ref().find(|(_, c)| *c == qc)?;
        first.get_or_insert(i);
        last = i;
    }
    let spread = last.saturating_sub(first.unwrap_or(0));
    Some(200u32.saturating_sub(spread.min(190) as u32))
}

/// The best `limit` candidates for `query`, best first (the label counts more than the
/// subtitle and the kind; ties keep the app's order).
pub fn rank<'a>(items: &'a [Candidate], query: &str, limit: usize) -> Vec<&'a Candidate> {
    let mut scored: Vec<(u32, usize, &Candidate)> = items
        .iter()
        .enumerate()
        .filter_map(|(i, c)| {
            let label = score(&c.mention.label, query).map(|s| s.saturating_mul(2));
            let other = score(&c.subtitle, query).max(score(&c.mention.kind, query)).filter(|_| !query.trim().is_empty());
            label.max(other).map(|s| (s, i, c))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.into_iter().take(limit).map(|(_, _, c)| c).collect()
}

/// Which picker the text at the cursor asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TriggerKind {
    /// `@` at the start of a word: the mention picker.
    Mention,
    /// `/` as the message's first character: the command list.
    Command,
}

/// The word being completed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Trigger {
    pub kind: TriggerKind,
    /// Character index of the `@` or `/`.
    pub start: usize,
    /// What is typed after it, up to the cursor.
    pub query: String,
}

/// The `@word` or leading `/word` that ends at `cursor` (a character index), if any. A mention
/// starts at the beginning of the text or after whitespace or an opening bracket (so an e-mail
/// address does not open the picker); a command only at the very start of the message.
pub fn trigger_at(text: &str, cursor: usize) -> Option<Trigger> {
    let before: Vec<char> = text.chars().take(cursor).collect();
    if before.len() < cursor {
        return None;
    }
    // the word runs back to the last whitespace
    let word_start = before.iter().rposition(|c| c.is_whitespace()).map_or(0, |i| i + 1);
    let word = before.get(word_start..)?;
    let first = *word.first()?;
    let query: String = word.get(1..).unwrap_or_default().iter().collect();
    if first == '/' && word_start == 0 && !query.contains('/') {
        return Some(Trigger { kind: TriggerKind::Command, start: 0, query });
    }
    // `@` inside the word (after a bracket) also counts, e-mail addresses don't
    let at = word.iter().rposition(|c| *c == '@')?;
    let ok = at == 0 || word.get(at - 1).is_some_and(|c| matches!(c, '(' | '[' | '{' | '"' | '\''));
    if !ok {
        return None;
    }
    Some(Trigger { kind: TriggerKind::Mention, start: word_start + at, query: word.get(at + 1..).unwrap_or_default().iter().collect() })
}

/// Replace the trigger's word (from its `@` or `/` to `cursor`) with `replacement` and a space.
/// Returns the new text and the cursor after the space.
pub fn complete(text: &str, trigger: &Trigger, cursor: usize, replacement: &str) -> (String, usize) {
    let chars: Vec<char> = text.chars().collect();
    let start = trigger.start.min(chars.len());
    let end = cursor.clamp(start, chars.len());
    let mut out: String = chars.get(..start).unwrap_or_default().iter().collect();
    out.push_str(replacement);
    let after: String = chars.get(end..).unwrap_or_default().iter().collect();
    if !after.starts_with(' ') {
        out.push(' ');
    }
    let new_cursor = out.chars().count() + usize::from(after.starts_with(' '));
    out.push_str(&after);
    (out, new_cursor)
}

/// Keep the mentions whose `@label` is still in `text` (the person may have deleted one).
pub fn retain_present(text: &str, mentions: &mut Vec<Mention>) {
    mentions.retain(|m| text.contains(&m.token()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn triggers() {
        assert_eq!(trigger_at("/mo", 3), Some(Trigger { kind: TriggerKind::Command, start: 0, query: "mo".into() }));
        assert_eq!(trigger_at("hi /mo", 6), None, "a command only at the start");
        assert_eq!(trigger_at("fix @lay", 8), Some(Trigger { kind: TriggerKind::Mention, start: 4, query: "lay".into() }));
        assert_eq!(trigger_at("@", 1), Some(Trigger { kind: TriggerKind::Mention, start: 0, query: String::new() }));
        assert_eq!(trigger_at("mail a@b.c", 10), None, "an address is not a mention");
        assert_eq!(trigger_at("(@x", 3).map(|t| t.start), Some(1));
        assert_eq!(trigger_at("@x done", 7), None, "the word ended");
        assert_eq!(trigger_at("héllo @é", 8).map(|t| t.query), Some("é".into()), "characters, not bytes");
        assert_eq!(trigger_at("short", 99), None, "a cursor past the end");
    }

    #[test]
    fn completing_replaces_the_word() {
        let t = trigger_at("make @ti bigger", 8).unwrap_or(Trigger { kind: TriggerKind::Mention, start: 0, query: String::new() });
        let (text, cur) = complete("make @ti bigger", &t, 8, "@Title");
        assert_eq!(text, "make @Title bigger");
        assert_eq!(cur, 12);
        let t = trigger_at("/he", 3).unwrap_or(Trigger { kind: TriggerKind::Command, start: 0, query: String::new() });
        assert_eq!(complete("/he", &t, 3, "/help"), ("/help ".into(), 6));
    }

    #[test]
    fn ranking_prefers_the_label_and_its_start() {
        let c = |label: &str, sub: &str| Candidate::new(Mention::new("layer", label, label), sub);
        let items = vec![c("Background", "Solid"), c("Title", "Text"), c("Subtitle", "Text"), c("Logo", "in Title comp")];
        let names: Vec<&str> = rank(&items, "tit", 10).iter().map(|c| c.mention.label.as_str()).collect();
        assert_eq!(names, ["Title", "Subtitle", "Logo"]);
        assert_eq!(rank(&items, "", 2).len(), 2, "empty query: the app's order");
        assert_eq!(rank(&items, "bgd", 10).first().map(|c| c.mention.label.as_str()), Some("Background"), "letters in order");
        assert!(rank(&items, "zzz", 10).is_empty());
    }

    #[test]
    fn mentions_reach_the_model_but_not_the_text() {
        let m = Mention::new("layer", "#2", "Title \"main\"").with_detail("comp: Main\nindex: 2");
        let msg = crate::types::Message::user_with_mentions("make @Title bigger", &[m.clone(), m.clone()]);
        assert_eq!(msg.text(), "make @Title bigger");
        assert_eq!(msg.mentions().count(), 1, "once each");
        assert_eq!(m.prompt_text(), "<mention kind=\"layer\" id=\"#2\" label=\"Title &quot;main&quot;\">\ncomp: Main\nindex: 2\n</mention>");
        let mut ms = vec![m.clone(), Mention::new("file", "/a.png", "a.png")];
        retain_present("move @a.png left", &mut ms);
        assert_eq!(ms.len(), 1);
    }
}
