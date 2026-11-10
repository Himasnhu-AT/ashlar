//! Chat kit: a transcript, message and tool-call views, and a composer with an `@` / `/` popup.
//!
//! The pieces draw plain data and report intent; they know nothing about where messages come
//! from. An app maps its own message type onto [`TranscriptItem`] (or uses [`ChatMessage`]) and
//! draws each row with [`Message`] and [`ToolCard`].
//!
//! ```
//! # use ashlar_ui::chat::{ChatMessage, Composer, Message, Transcript};
//! # fn ui(ui: &mut egui::Ui, messages: &[ChatMessage], revision: u64, draft: &mut String) {
//! Transcript::new("chat", messages, revision).stick_to_bottom(true).show(ui, |ui, m| {
//!     let id = format!("chat.msg.{}", m.id);
//!     Message::new(&id, m.role, &m.text).streaming(m.streaming).show(ui);
//! });
//! let out = Composer::new("chat.input", draft).placeholder("Ask anything").show(ui);
//! if let Some(text) = out.submitted {
//!     // send `text`
//! #   let _ = text;
//! }
//! # }
//! ```
//!
//! Automation ids: `<transcript>` (a list) and `<transcript>.jump`; `<card>` (a status) with
//! `<card>.toggle`, `<card>.approve` and `<card>.deny`; `<composer>` (a field) and
//! `<composer>.popup.item.<n>` for the suggestions on screen.

mod composer;
mod message;
mod popup_list;
mod tool_card;
mod transcript;

#[cfg(test)]
mod tests;

pub use composer::{Composer, ComposerResponse, Trigger};
pub use message::Message;
pub use popup_list::PopupList;
pub use tool_card::{Approval, ToolCard, ToolCardResponse, ToolStatus};
pub use transcript::{MAX_MESSAGES, Transcript, TranscriptItem, TranscriptResponse};

/// Who wrote a message.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ChatRole {
    #[default]
    User,
    Assistant,
    System,
    Tool,
}

impl ChatRole {
    /// The role's name in `labels`.
    pub fn label(self, labels: &ChatLabels) -> &str {
        match self {
            ChatRole::User => &labels.user,
            ChatRole::Assistant => &labels.assistant,
            ChatRole::System => &labels.system,
            ChatRole::Tool => &labels.tool,
        }
    }
}

/// A ready-made message record for apps that have none of their own.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChatMessage {
    /// Stable for the message's life (keys its cached height).
    pub id: u64,
    pub role: ChatRole,
    /// Markdown.
    pub text: String,
    /// Still receiving text.
    pub streaming: bool,
    /// Bumped whenever `text` (or anything else that changes its height) changes.
    pub revision: u64,
}

impl TranscriptItem for ChatMessage {
    fn key(&self) -> u64 {
        self.id
    }
    fn revision(&self) -> u64 {
        self.revision
    }
}

/// Words the chat kit shows (localized by the app).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatLabels {
    /// The transcript's accessible name.
    pub transcript: String,
    pub jump_to_latest: String,
    pub user: String,
    pub assistant: String,
    pub system: String,
    pub tool: String,
    /// Shown under a message that is still streaming.
    pub streaming: String,
    pub approve: String,
    pub deny: String,
    pub show_details: String,
    pub hide_details: String,
    pub pending: String,
    pub running: String,
    pub succeeded: String,
    pub failed: String,
    pub needs_approval: String,
    pub approved: String,
    pub denied: String,
    /// The composer's popup list.
    pub suggestions: String,
    /// The composer's accessible name.
    pub composer: String,
}

impl Default for ChatLabels {
    fn default() -> Self {
        ChatLabels {
            transcript: "Conversation".into(),
            jump_to_latest: "Jump to latest".into(),
            user: "You".into(),
            assistant: "Assistant".into(),
            system: "System".into(),
            tool: "Tool".into(),
            streaming: "Writing…".into(),
            approve: "Approve".into(),
            deny: "Deny".into(),
            show_details: "Show details".into(),
            hide_details: "Hide details".into(),
            pending: "Pending".into(),
            running: "Running".into(),
            succeeded: "Done".into(),
            failed: "Failed".into(),
            needs_approval: "Needs approval".into(),
            approved: "Approved".into(),
            denied: "Denied".into(),
            suggestions: "Suggestions".into(),
            composer: "Message".into(),
        }
    }
}
