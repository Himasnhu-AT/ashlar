//! Making a long conversation short (`/compact`) and naming a chat.
//!
//! [`summarize`] sends the conversation, written out as a plain transcript ([`transcript`]: tool
//! results cut, images named), to the model with no tools and asks for a summary that keeps what
//! the work needs; [`compacted`] turns it into a new two-message conversation that the next
//! message continues. [`make_title`] asks for a few words for the history list; [`fallback_title`]
//! names a chat from its first message without a request. [`estimate_tokens`] is a rough size for
//! "this chat is getting long" hints.
//!
//! These block: run them on a worker thread.

use crate::error::Error;
use crate::provider::{Cancel, Provider, Request};
use crate::types::{Block, Conversation, Message, Role, Thinking, Usage};

/// The system prompt of a summary request.
pub const SUMMARY_SYSTEM: &str = "You write the summary that replaces an earlier part of a conversation between a person and an assistant working inside an app. The assistant will continue from your summary alone, so keep everything the work needs and nothing else.";

/// What the summary must keep.
pub const SUMMARY_INSTRUCTIONS: &str = "Summarise the conversation above for the assistant to continue from. Keep: what the person asked for and still wants, decisions made and why, the exact ids, names, paths and values of things created or changed (and their current state), what was tried and failed, and what is left to do. Leave out greetings, repetition and raw tool output. Use short headings and bullet points.";

/// How a compacted conversation's first message starts.
pub const SUMMARY_PREFIX: &str = "This chat was compacted. Summary of the conversation so far:";

/// Longest tool result kept in a transcript, in bytes.
pub const TRANSCRIPT_RESULT_BYTES: usize = 2_000;

fn cut(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    format!("{} … ({} more bytes)", s.get(..end).unwrap_or_default(), s.len().saturating_sub(end))
}

/// The conversation as plain text (who said what, which tools ran with which arguments and what
/// they returned, cut to [`TRANSCRIPT_RESULT_BYTES`]), at most `max_bytes` long: when longer,
/// its start and the most recent part are kept.
pub fn transcript(conversation: &Conversation, max_bytes: usize) -> String {
    let mut parts: Vec<String> = Vec::new();
    for m in conversation.messages() {
        for b in &m.blocks {
            let line = match (m.role, b) {
                (Role::User, Block::Text { text }) => format!("Person: {text}"),
                (Role::Assistant, Block::Text { text }) if !text.trim().is_empty() => format!("Assistant: {text}"),
                (_, Block::Mention(x)) => format!("(mentioned {} `{}` id {})", x.kind, x.label, x.id),
                (_, Block::ToolCall(c)) => format!("Assistant ran `{}` {}", c.name, cut(&c.input.to_string(), 600)),
                (_, Block::ToolResult(r)) => {
                    let images = if r.images.is_empty() { String::new() } else { format!(" [{} image(s)]", r.images.len()) };
                    format!("{}{images}: {}", if r.is_error { "Tool error" } else { "Tool result" }, cut(&r.content, TRANSCRIPT_RESULT_BYTES))
                }
                _ => continue,
            };
            parts.push(line);
        }
    }
    let all = parts.join("\n\n");
    if all.len() <= max_bytes {
        return all;
    }
    // the start (the original request) and as much of the end as fits
    let head_budget = max_bytes / 4;
    let head = cut(&all, head_budget);
    let tail_budget = max_bytes.saturating_sub(head.len()).saturating_sub(40);
    let mut start = all.len().saturating_sub(tail_budget);
    while !all.is_char_boundary(start) {
        start = start.saturating_add(1);
    }
    format!("{head}\n\n[… earlier messages left out …]\n\n{}", all.get(start..).unwrap_or_default())
}

/// Ask `provider` for a summary of `conversation` (no tools, up to 4 000 tokens).
pub fn summarize(provider: &mut dyn Provider, model: &str, conversation: &Conversation, cancel: &Cancel) -> Result<(String, Usage), Error> {
    let text = format!("<conversation>\n{}\n</conversation>\n\n{SUMMARY_INSTRUCTIONS}", transcript(conversation, 400_000));
    let messages = [Message::user_text(text)];
    let r = Request { model, system: SUMMARY_SYSTEM, tools: &[], messages: &messages, max_tokens: 4_000, effort: None, thinking: Thinking::Default };
    let reply = provider.send(&r, &mut |_| {}, cancel)?;
    let summary = reply.message.text().trim().to_string();
    if summary.is_empty() {
        return Err(Error::Other("The model returned an empty summary.".into()));
    }
    Ok((summary, reply.usage))
}

/// A new conversation that starts from `summary`: the person's message carrying it and the
/// assistant's acknowledgement, so the next message is the person's as usual.
pub fn compacted(summary: &str) -> Conversation {
    let mut c = Conversation::new();
    c.push_user_text(format!("{SUMMARY_PREFIX}\n\n{}", summary.trim()));
    c.push(Message {
        role: Role::Assistant,
        blocks: vec![Block::Text { text: "Understood. I'll continue from this summary.".into() }],
        native: None,
    });
    c
}

/// Whether `conversation` is a compacted one (starts with a summary).
pub fn is_compacted(conversation: &Conversation) -> bool {
    conversation.messages().first().is_some_and(|m| m.role == Role::User && m.text().starts_with(SUMMARY_PREFIX))
}

/// A title from the first message: its first line, at most 48 characters, cut at a word.
pub fn fallback_title(text: &str) -> String {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("New chat");
    let line = line.trim_start_matches('/').trim();
    let max = 48;
    if line.chars().count() <= max {
        return if line.is_empty() { "New chat".into() } else { line.to_string() };
    }
    let mut out: String = line.chars().take(max).collect();
    if let Some(i) = out.rfind(' ').filter(|i| *i > 16) {
        out.truncate(i);
    }
    out.push('…');
    out
}

/// Ask for a title of three to six words for a chat that starts with `first_message`. Falls
/// back to nothing: the caller keeps [`fallback_title`] on an error.
pub fn make_title(provider: &mut dyn Provider, model: &str, first_message: &str, cancel: &Cancel) -> Result<String, Error> {
    let text = format!(
        "Write a title of three to six words for a chat that starts with the message below. Reply with the title only: no quotes, no full stop.\n\n<message>\n{}\n</message>",
        cut(first_message, 4_000)
    );
    let messages = [Message::user_text(text)];
    let r = Request { model, system: "You name chats.", tools: &[], messages: &messages, max_tokens: 60, effort: None, thinking: Thinking::Default };
    let reply = provider.send(&r, &mut |_| {}, cancel)?;
    let t = clean_title(&reply.message.text());
    if t.is_empty() { Err(Error::Other("The model returned no title.".into())) } else { Ok(t) }
}

/// A model's title made tidy: one line, no quotes or trailing full stop, at most 60 characters.
pub fn clean_title(raw: &str) -> String {
    let line = raw.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or_default();
    let line = line.trim_start_matches("Title:").trim().trim_matches(|c| matches!(c, '"' | '\'' | '*' | '#' | '`')).trim().trim_end_matches('.');
    line.chars().take(60).collect::<String>().trim().to_string()
}

/// A rough token count of the conversation (four bytes a token, about 1 600 a picture).
pub fn estimate_tokens(conversation: &Conversation) -> u64 {
    let mut bytes = 0u64;
    let mut images = 0u64;
    for m in conversation.messages() {
        for b in &m.blocks {
            let n = match b {
                Block::Text { text } | Block::Thinking { text } => text.len(),
                Block::ToolCall(c) => c.input.to_string().len().saturating_add(c.name.len()),
                Block::ToolResult(r) => {
                    images = images.saturating_add(r.images.len() as u64);
                    r.content.len()
                }
                Block::Mention(x) => x.prompt_text().len(),
            };
            bytes = bytes.saturating_add(n as u64);
        }
    }
    (bytes / 4).saturating_add(images.saturating_mul(1_600))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::{MockProvider, MockReply};
    use crate::types::{ToolCall, ToolResult};
    use serde_json::json;

    fn sample() -> Conversation {
        let mut c = Conversation::new();
        c.push(Message::user_with_mentions("make @Title red", &[crate::mention::Mention::new("layer", "#2", "Title")]));
        let call = ToolCall { id: "a".into(), name: "execute".into(), input: json!({"command": "layer.set"}), invalid_input: None };
        c.push(Message { role: Role::Assistant, blocks: vec![Block::Text { text: "On it.".into() }, Block::ToolCall(call)], native: None });
        c.push(Message::tool_results(vec![ToolResult { call_id: "a".into(), content: "x".repeat(5_000), ..Default::default() }]));
        c.push(Message { role: Role::Assistant, blocks: vec![Block::Text { text: "Done.".into() }], native: None });
        c
    }

    #[test]
    fn the_transcript_names_everything_and_cuts_results() {
        let t = transcript(&sample(), 100_000);
        assert!(t.starts_with("Person: make @Title red"));
        assert!(t.contains("(mentioned layer `Title` id #2)"));
        assert!(t.contains("Assistant ran `execute` {\"command\":\"layer.set\"}"));
        assert!(t.contains("(3000 more bytes)"));
        assert!(t.ends_with("Assistant: Done."));
        let short = transcript(&sample(), 600);
        assert!(short.len() < 700, "{}", short.len());
        assert!(short.starts_with("Person:") && short.ends_with("Done."));
    }

    #[test]
    fn summarising_makes_a_short_conversation() {
        let mut p = MockProvider::new(vec![MockReply::text("## Asked\n- Title red")]);
        let (s, _) = summarize(&mut p, "m", &sample(), &Cancel::new()).unwrap_or_default();
        assert_eq!(s, "## Asked\n- Title red");
        let c = compacted(&s);
        assert_eq!(c.len(), 2);
        assert!(is_compacted(&c));
        assert!(!is_compacted(&sample()));
        assert!(estimate_tokens(&sample()) > estimate_tokens(&c));
    }

    #[test]
    fn titles() {
        assert_eq!(
            fallback_title("\n  make the title red and move it to the left a little please\nmore"),
            "make the title red and move it to the left a…"
        );
        assert_eq!(fallback_title("/review the comp"), "review the comp");
        assert_eq!(fallback_title("  "), "New chat");
        assert_eq!(clean_title("Title: \"Red title, moved left.\"\n"), "Red title, moved left");
        let mut p = MockProvider::new(vec![MockReply::text("**Red title tweak**")]);
        assert_eq!(make_title(&mut p, "m", "make it red", &Cancel::new()).ok().as_deref(), Some("Red title tweak"));
    }
}
