//! `/` commands: the built-ins every chat has ([`Builtin`]), the app's own, and the person's
//! saved prompts (Markdown files in a folder, one command per file, like custom slash commands
//! in coding assistants). [`parse`] reads `/name args` from a message, [`SlashRegistry`] finds
//! and ranks commands, [`expand`] fills a saved prompt with the arguments.
//!
//! Running a command is the app's: a [`Builtin`] maps to one of its commands (so it is
//! scriptable), a [`SlashAction::Prompt`] is sent as the person's message after [`expand`].
//!
//! A saved prompt file `review.md` is the command `/review`:
//!
//! ```text
//! ---
//! description: Check the open project for mistakes
//! argument-hint: [what to look at]
//! ---
//! Review $ARGUMENTS and list what looks wrong.
//! ```

use std::path::Path;

use serde::{Deserialize, Serialize};

/// The commands every chat has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Builtin {
    /// A new chat; the current one stays in history.
    New,
    /// Start over with an empty chat (an alias of `New`, as in coding assistants).
    Clear,
    /// Show the saved chats (`args`: a search).
    History,
    /// Show or set the model (`args`: its name).
    Model,
    /// Show or set the thinking effort (`args`: low … max).
    Effort,
    /// Summarise the conversation so far into a shorter one, to save tokens.
    Compact,
    /// This chat's tokens and cost.
    Cost,
    /// The commands.
    Help,
    /// Open or attach a docs page (`args`: its name).
    Docs,
}

impl Builtin {
    pub const ALL: [Builtin; 9] = [
        Builtin::New,
        Builtin::Clear,
        Builtin::History,
        Builtin::Model,
        Builtin::Effort,
        Builtin::Compact,
        Builtin::Cost,
        Builtin::Help,
        Builtin::Docs,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Builtin::New => "new",
            Builtin::Clear => "clear",
            Builtin::History => "history",
            Builtin::Model => "model",
            Builtin::Effort => "effort",
            Builtin::Compact => "compact",
            Builtin::Cost => "cost",
            Builtin::Help => "help",
            Builtin::Docs => "docs",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Builtin::New => "Start a new chat (this one stays in history)",
            Builtin::Clear => "Start over with an empty chat",
            Builtin::History => "Open a saved chat",
            Builtin::Model => "Show or change the model",
            Builtin::Effort => "Show or change how hard the model thinks",
            Builtin::Compact => "Summarise this chat to save tokens",
            Builtin::Cost => "Tokens and cost of this chat",
            Builtin::Help => "List the commands",
            Builtin::Docs => "Attach a docs page",
        }
    }

    pub fn args(self) -> Option<&'static str> {
        match self {
            Builtin::History => Some("[search]"),
            Builtin::Model => Some("[name]"),
            Builtin::Effort => Some("[low|medium|high|xhigh|max]"),
            Builtin::Docs => Some("<page>"),
            _ => None,
        }
    }
}

/// What a command does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum SlashAction {
    Builtin {
        builtin: Builtin,
    },
    /// A saved prompt: [`expand`] it with the arguments and send it.
    Prompt {
        body: String,
    },
    /// One of the app's own (its id, for the app to run).
    App {
        id: String,
    },
}

/// One command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlashCommand {
    /// Without the slash: `compact`, `review`.
    pub name: String,
    pub description: String,
    /// What may follow the name (`[search]`), for the list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<String>,
    pub action: SlashAction,
}

impl SlashCommand {
    pub fn builtin(b: Builtin) -> Self {
        Self {
            name: b.name().into(),
            description: b.description().into(),
            args: b.args().map(Into::into),
            action: SlashAction::Builtin { builtin: b },
        }
    }

    pub fn app(name: impl Into<String>, description: impl Into<String>, id: impl Into<String>) -> Self {
        Self { name: name.into(), description: description.into(), args: None, action: SlashAction::App { id: id.into() } }
    }

    /// A saved prompt from a file's text (front matter `description` and `argument-hint`
    /// optional; without a description, the body's first line).
    pub fn prompt(name: impl Into<String>, file_text: &str) -> Self {
        let (front, body) = split_front_matter(file_text);
        let field = |k: &str| {
            front.lines().find_map(|l| {
                let (key, v) = l.split_once(':')?;
                (key.trim() == k).then(|| v.trim().trim_matches('"').trim_matches('\'').to_string())
            })
        };
        let description = field("description")
            .filter(|d| !d.is_empty())
            .unwrap_or_else(|| first_words(body.lines().find(|l| !l.trim().is_empty()).unwrap_or("Saved prompt"), 60));
        Self {
            name: name.into(),
            description,
            args: field("argument-hint").filter(|a| !a.is_empty()),
            action: SlashAction::Prompt { body: body.trim().to_string() },
        }
    }

    /// `/name [args]`, for the list.
    pub fn usage(&self) -> String {
        match &self.args {
            Some(a) => format!("/{} {a}", self.name),
            None => format!("/{}", self.name),
        }
    }
}

fn split_front_matter(text: &str) -> (&str, &str) {
    let t = text.strip_prefix('\u{feff}').unwrap_or(text);
    let Some(rest) = t.strip_prefix("---").filter(|r| r.starts_with('\n') || r.starts_with("\r\n")) else { return ("", t) };
    match rest.find("\n---") {
        Some(end) => {
            let front = rest.get(..end).unwrap_or_default();
            let after = rest.get(end + 4..).unwrap_or_default();
            (front, after.split_once('\n').map_or("", |(_, b)| b))
        }
        None => ("", t),
    }
}

fn first_words(line: &str, max: usize) -> String {
    let line = line.trim().trim_start_matches('#').trim();
    if line.chars().count() <= max {
        return line.to_string();
    }
    let mut out: String = line.chars().take(max).collect();
    if let Some(i) = out.rfind(' ') {
        out.truncate(i);
    }
    out.push('…');
    out
}

/// A valid command name: letters, digits, `-`, `_`, `.` and `:` (no spaces, no slashes).
pub fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 64 && name.chars().all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
}

/// A `/name args` message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Invocation<'a> {
    pub name: &'a str,
    /// The rest, trimmed (may be empty).
    pub args: &'a str,
}

/// Read `/name args` from a message; `None` when it isn't a command (`//text` is a message that
/// starts with a slash, `/` alone or `/ x` is text).
pub fn parse(input: &str) -> Option<Invocation<'_>> {
    let rest = input.trim_start().strip_prefix('/')?;
    let (name, args) = match rest.find(char::is_whitespace) {
        Some(i) => (rest.get(..i)?, rest.get(i..)?.trim()),
        None => (rest, ""),
    };
    valid_name(name).then_some(Invocation { name, args })
}

/// Fill a saved prompt: `$ARGUMENTS` is all of them, `$1` … `$9` one word each. A prompt with no
/// placeholder gets the arguments appended as a new paragraph.
pub fn expand(body: &str, args: &str) -> String {
    let args = args.trim();
    let has_numbered = (1..=9).any(|i| body.contains(&format!("${i}")));
    if !body.contains("$ARGUMENTS") && !has_numbered {
        return if args.is_empty() { body.to_string() } else { format!("{body}\n\n{args}") };
    }
    let mut out = body.replace("$ARGUMENTS", args);
    let words: Vec<&str> = args.split_whitespace().collect();
    for i in (1..=9).rev() {
        out = out.replace(&format!("${i}"), words.get(i - 1).copied().unwrap_or_default());
    }
    out
}

/// The commands a chat offers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlashRegistry {
    commands: Vec<SlashCommand>,
}

/// The largest saved-prompt file read.
pub const MAX_PROMPT_BYTES: u64 = 64 * 1024;
/// The most saved prompts read from one folder.
pub const MAX_PROMPTS: usize = 200;

impl SlashRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Every [`Builtin`].
    pub fn with_builtins() -> Self {
        Self { commands: Builtin::ALL.iter().map(|b| SlashCommand::builtin(*b)).collect() }
    }

    /// Add a command; one with the same name is replaced. Invalid names are ignored (`false`).
    pub fn add(&mut self, c: SlashCommand) -> bool {
        if !valid_name(&c.name) {
            return false;
        }
        match self.commands.iter_mut().find(|x| x.name == c.name) {
            Some(x) => *x = c,
            None => self.commands.push(c),
        }
        true
    }

    /// Remove every command whose action is a saved prompt (before loading them again).
    pub fn clear_prompts(&mut self) {
        self.commands.retain(|c| !matches!(c.action, SlashAction::Prompt { .. }));
    }

    /// Add each `*.md` file in `dir` (not its subfolders) as a saved prompt named after the file
    /// (lowercase). A prompt never replaces a built-in or an app command. A missing folder, an
    /// unreadable or oversized file is skipped. Returns how many were added.
    pub fn load_prompts(&mut self, dir: &Path) -> usize {
        let Ok(entries) = std::fs::read_dir(dir) else { return 0 };
        let mut files: Vec<_> = entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("md")) && p.is_file())
            .collect();
        files.sort();
        let mut n = 0;
        for p in files.into_iter().take(MAX_PROMPTS) {
            let Some(name) = p.file_stem().and_then(|s| s.to_str()).map(str::to_lowercase) else { continue };
            if name.starts_with('_') || self.get(&name).is_some_and(|c| !matches!(c.action, SlashAction::Prompt { .. })) {
                continue;
            }
            if std::fs::metadata(&p).map(|m| m.len()).unwrap_or(u64::MAX) > MAX_PROMPT_BYTES {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&p) else { continue };
            if self.add(SlashCommand::prompt(name, &text)) {
                n += 1;
            }
        }
        n
    }

    pub fn get(&self, name: &str) -> Option<&SlashCommand> {
        self.commands.iter().find(|c| c.name.eq_ignore_ascii_case(name))
    }

    pub fn all(&self) -> &[SlashCommand] {
        &self.commands
    }

    /// The commands for the list while `/query` is typed, best first.
    pub fn matching(&self, query: &str, limit: usize) -> Vec<&SlashCommand> {
        let mut scored: Vec<(u32, usize, &SlashCommand)> = self
            .commands
            .iter()
            .enumerate()
            .filter_map(|(i, c)| {
                let s = crate::mention::score(&c.name, query).map(|s| s.saturating_mul(2));
                // the description helps only when the query is a word, not a prefix
                let d = (query.len() > 2).then(|| crate::mention::score(&c.description, query).filter(|s| *s >= 400)).flatten();
                s.max(d).map(|s| (s, i, c))
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        scored.into_iter().take(limit).map(|(_, _, c)| c).collect()
    }

    /// `/help`'s text: one line per command.
    pub fn help_text(&self) -> String {
        let mut s = String::from("Commands:\n");
        for c in &self.commands {
            s.push_str(&format!("- `{}` — {}\n", c.usage(), c.description));
        }
        s.push_str("\nType @ to mention something in the project.");
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parsing() {
        assert_eq!(parse("/model claude-opus-5-5"), Some(Invocation { name: "model", args: "claude-opus-5-5" }));
        assert_eq!(parse("  /new"), Some(Invocation { name: "new", args: "" }));
        assert_eq!(parse("/docs  motion  basics "), Some(Invocation { name: "docs", args: "motion  basics" }));
        assert_eq!(parse("//not a command"), None);
        assert_eq!(parse("/"), None);
        assert_eq!(parse("/ x"), None);
        assert_eq!(parse("hello /new"), None);
        assert_eq!(parse("/a/b"), None);
    }

    #[test]
    fn expanding() {
        assert_eq!(expand("Review $ARGUMENTS now", "the title"), "Review the title now");
        assert_eq!(expand("Move $1 to $2", "logo left extra"), "Move logo to left");
        assert_eq!(expand("Summarise", ""), "Summarise");
        assert_eq!(expand("Summarise", "briefly"), "Summarise\n\nbriefly");
        assert_eq!(expand("$1 and $10", "a"), "a and a0", "$1 is replaced inside $10 too: keep to $1–$9");
    }

    #[test]
    fn saved_prompts_from_a_folder() {
        let dir = std::env::temp_dir().join(format!("ashlar-slash-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join("Review.md"), "---\ndescription: Check the project\nargument-hint: [what]\n---\nReview $ARGUMENTS.\n");
        let _ = std::fs::write(dir.join("plain.md"), "# Tidy up the layers\nRename them sensibly.");
        let _ = std::fs::write(dir.join("help.md"), "Not the built-in");
        let _ = std::fs::write(dir.join("notes.txt"), "ignored");
        let mut r = SlashRegistry::with_builtins();
        assert_eq!(r.load_prompts(&dir), 2);
        let review = r.get("review").cloned();
        assert_eq!(review.as_ref().map(|c| c.description.as_str()), Some("Check the project"));
        assert_eq!(review.as_ref().map(|c| c.usage()), Some("/review [what]".into()));
        assert_eq!(review.map(|c| c.action), Some(SlashAction::Prompt { body: "Review $ARGUMENTS.".into() }));
        assert_eq!(r.get("plain").map(|c| c.description.as_str()), Some("Tidy up the layers"));
        assert!(matches!(r.get("help").map(|c| &c.action), Some(SlashAction::Builtin { .. })), "a prompt never replaces a built-in");
        assert_eq!(r.load_prompts(&dir.join("missing")), 0);
        r.clear_prompts();
        assert!(r.get("review").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn matching_and_help() {
        let mut r = SlashRegistry::with_builtins();
        r.add(SlashCommand::app("render", "Render the active comp", "render.queue"));
        assert_eq!(r.matching("co", 10).iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["compact", "cost"]);
        assert_eq!(r.matching("", 100).len(), 10);
        assert_eq!(r.matching("summar", 3).first().map(|c| c.name.as_str()), Some("compact"), "by description");
        assert!(r.help_text().contains("`/effort [low|medium|high|xhigh|max]`"));
        assert!(!r.add(SlashCommand::app("bad name", "x", "y")));
    }
}
