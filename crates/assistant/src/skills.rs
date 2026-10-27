//! Skills for the assistant, disclosed progressively: the system prompt lists each skill by name
//! and description only ([`Skills::prompt_section`]); the model loads one's full text with the
//! `load_skill` tool when a task needs it, and reads the files that come with it with
//! `read_skill_file` ([`SkillTools`]). The person can pick one too: as a `/name` command
//! ([`Skills::slash_commands`]), whose text then goes into that message as a
//! [`Block::Mention`](crate::Block::Mention) of kind `skill` ([`Skills::picked`]).
//!
//! Skills come from anywhere a `SKILL.md` can: an app's docs category ([`Skills::add_library`]), a
//! folder of skill folders ([`Skills::add_dir`], the person's own skills), skills made in code
//! ([`Skills::add_skill`]), or an integration ([`crate::integration`], feature `integrations`),
//! namespaced `<integration>:<name>`. A later skill with the same name replaces an earlier one, so
//! add the app's first and the person's after.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ashlar_agents::skills::{self as agent_skills, Skill};
use serde_json::{Value, json};

use crate::agent::{ToolHost, ToolOutput};
use crate::mention::Mention;
use crate::provider::Cancel;
use crate::slash::{SlashAction, SlashCommand};
use crate::types::{ToolCall, ToolSpec};

/// The tool that loads a skill's text.
pub const LOAD_SKILL: &str = "load_skill";
/// The tool that reads a file that comes with a skill.
pub const READ_SKILL_FILE: &str = "read_skill_file";
/// The largest skill file read.
pub const MAX_FILE_BYTES: u64 = 256 * 1024;
/// The most text a picked skill adds to a message.
pub const MAX_PICKED: usize = 48_000;

/// Loads a skill's text when asked (an integration's resource).
pub type Fetch = Arc<dyn Fn() -> Result<String, String> + Send + Sync>;

/// Where a skill's text is.
#[derive(Clone)]
pub enum SkillContent {
    Text(String),
    /// A skill folder: its `SKILL.md` body and the other files in it.
    Folder {
        dir: PathBuf,
        body: String,
        files: Vec<String>,
    },
    /// Fetched when loaded (it may block: tools run on the turn's thread).
    Fetch(Fetch),
}

impl std::fmt::Debug for SkillContent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SkillContent::Text(t) => f.debug_tuple("Text").field(&t.len()).finish(),
            SkillContent::Folder { dir, files, .. } => f.debug_struct("Folder").field("dir", dir).field("files", files).finish(),
            SkillContent::Fetch(_) => f.write_str("Fetch"),
        }
    }
}

/// One skill.
#[derive(Debug, Clone)]
pub struct SkillEntry {
    /// `lower-thirds`, or `<integration>:<name>`.
    pub name: String,
    /// When to use it (what the system prompt lists).
    pub description: String,
    /// Where it comes from, for people (`built-in`, `yours`, `earshot`).
    pub source: String,
    pub content: SkillContent,
}

/// The skills a chat offers.
#[derive(Debug, Clone, Default)]
pub struct Skills {
    entries: Vec<SkillEntry>,
}

/// A skill name as commands take it: [`agent_skills::name`] for each `:`-separated part.
pub fn skill_name(s: &str) -> String {
    s.split(':').map(agent_skills::name).filter(|p| !p.is_empty()).collect::<Vec<_>>().join(":")
}

impl Skills {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add one; one with the same name is replaced. A nameless or undescribed one is ignored.
    pub fn add(&mut self, mut e: SkillEntry) -> bool {
        e.name = skill_name(&e.name);
        e.description = e.description.split_whitespace().collect::<Vec<_>>().join(" ");
        if e.name.is_empty() || e.description.is_empty() {
            return false;
        }
        match self.entries.iter_mut().find(|x| x.name == e.name) {
            Some(x) => *x = e,
            None => self.entries.push(e),
        }
        self.entries.sort_by(|a, b| a.name.cmp(&b.name));
        true
    }

    /// Add a skill made in code.
    pub fn add_skill(&mut self, s: Skill, source: &str) -> bool {
        self.add(SkillEntry { name: s.name, description: s.description, source: source.into(), content: SkillContent::Text(s.body) })
    }

    /// Add the skills under `dir` ([`agent_skills::load_dir`]), their names prefixed with
    /// `prefix:` when it isn't empty. Returns how many.
    pub fn add_dir(&mut self, dir: &Path, prefix: &str, source: &str) -> usize {
        let mut n = 0;
        for s in agent_skills::load_dir(dir) {
            let name = if prefix.is_empty() { s.skill.name.clone() } else { format!("{prefix}:{}", s.skill.name) };
            let entry = SkillEntry {
                name,
                description: s.skill.description,
                source: source.into(),
                content: SkillContent::Folder { dir: s.dir, body: s.skill.body, files: s.files },
            };
            if self.add(entry) {
                n += 1;
            }
        }
        n
    }

    /// Add every page of `category` in `lib` (and its subcategories) that has a description, named
    /// after its path in the category (`skills/titles/lower-thirds.md` is `titles-lower-thirds`;
    /// `skills/x/SKILL.md` is its frontmatter `name`, or `x`). Returns how many.
    pub fn add_library(&mut self, lib: &ashlar_docs::Library, category: &str, source: &str) -> usize {
        let cat = category.trim_matches('/');
        let mut n = 0;
        for doc in lib.list(Some(cat), None, true) {
            let rel = doc.path.strip_prefix(cat).unwrap_or(&doc.path).trim_start_matches('/');
            let stem = rel.strip_suffix(".md").unwrap_or(rel);
            let stem = stem.strip_suffix("/SKILL").unwrap_or(stem);
            let Some(skill) = agent_skills::parse(&doc.text, stem) else { continue };
            let body = agent_skills::rewrite_links(&skill.body, &doc.category);
            if self.add(SkillEntry { name: skill.name, description: skill.description, source: source.into(), content: SkillContent::Text(body) }) {
                n += 1;
            }
        }
        n
    }

    pub fn entries(&self) -> &[SkillEntry] {
        &self.entries
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn get(&self, name: &str) -> Option<&SkillEntry> {
        let n = skill_name(name);
        self.entries.iter().find(|e| e.name == n)
    }

    /// What the system prompt says about the skills: how to use them and one line each (name and
    /// description). Empty without skills. Stable for a given set (sorted), so it caches.
    pub fn prompt_section(&self) -> String {
        if self.entries.is_empty() {
            return String::new();
        }
        let mut s = format!(
            "# Skills\n\nSkills are instructions for particular kinds of work. When a request matches a skill's description, call `{LOAD_SKILL}` with its name before you start and follow what it says; load only the skills the task needs. A skill may come with files: read them with `{READ_SKILL_FILE}`. When the person's message carries a skill they picked (a `skill` mention), follow it.\n\n"
        );
        for e in &self.entries {
            s.push_str(&format!("- `{}`: {}\n", e.name, e.description));
        }
        s
    }

    /// A skill's full text, with the list of its files.
    pub fn load(&self, name: &str) -> Result<String, String> {
        let e = self.get(name).ok_or_else(|| format!("There is no skill named `{name}`. The skills are listed in the system prompt."))?;
        let mut text = match &e.content {
            SkillContent::Text(t) => t.clone(),
            SkillContent::Folder { body, .. } => body.clone(),
            SkillContent::Fetch(f) => f()?,
        };
        if let SkillContent::Folder { files, .. } = &e.content
            && !files.is_empty()
        {
            text.push_str(&format!("\n\n---\nFiles in this skill (read one with `{READ_SKILL_FILE}`): {}", files.join(", ")));
        }
        Ok(format!("# Skill `{}`\n\n{}", e.name, text.trim()))
    }

    /// A file that comes with a skill (a path it lists), as text.
    pub fn read_file(&self, name: &str, path: &str) -> Result<String, String> {
        let e = self.get(name).ok_or_else(|| format!("There is no skill named `{name}`."))?;
        let SkillContent::Folder { dir, files, .. } = &e.content else { return Err(format!("The skill `{name}` has no files.")) };
        let path = path.trim().trim_start_matches("./");
        if !files.iter().any(|f| f == path) {
            return Err(format!("`{path}` isn't one of the skill's files: {}", files.join(", ")));
        }
        let full = dir.join(path);
        let size = std::fs::metadata(&full).map(|m| m.len()).map_err(|e| format!("{path}: {e}"))?;
        if size > MAX_FILE_BYTES {
            return Err(format!("{path} is {size} bytes, more than this tool reads ({MAX_FILE_BYTES})."));
        }
        let bytes = std::fs::read(&full).map_err(|e| format!("{path}: {e}"))?;
        String::from_utf8(bytes).map_err(|_| format!("{path} isn't a text file."))
    }

    /// Each skill as a `/name` command.
    pub fn slash_commands(&self) -> Vec<SlashCommand> {
        self.entries
            .iter()
            .map(|e| SlashCommand {
                name: e.name.clone(),
                description: e.description.clone(),
                args: Some("[what to do]".into()),
                action: SlashAction::Skill { name: e.name.clone() },
            })
            .collect()
    }

    /// The person picked a skill for their message: a `skill` mention carrying its text (or, for
    /// a skill fetched on demand, the instruction to load it), so it goes into that turn only.
    pub fn picked(&self, name: &str) -> Result<Mention, String> {
        let e = self.get(name).ok_or_else(|| format!("There is no skill named `{name}`."))?;
        let detail = match &e.content {
            SkillContent::Fetch(_) => format!("The person picked this skill for this message. Load it with `{LOAD_SKILL}` and follow it."),
            _ => {
                let text = self.load(&e.name)?;
                let mut cut: String = text.chars().take(MAX_PICKED).collect();
                if cut.len() < text.len() {
                    cut.push_str(&format!("\n… (cut; load the rest with `{LOAD_SKILL}`)"));
                }
                format!("The person picked this skill for this message: follow it.\n\n{cut}")
            }
        };
        Ok(Mention::new("skill", e.name.clone(), e.name.clone()).with_detail(detail))
    }

    /// The tools that load skills, over a shared catalog.
    pub fn tools(self: &Arc<Self>) -> SkillTools {
        SkillTools(Arc::clone(self))
    }
}

/// `load_skill` and `read_skill_file`, as a tool host (combine it with the app's tools).
#[derive(Debug, Clone)]
pub struct SkillTools(pub Arc<Skills>);

impl ToolHost for SkillTools {
    fn tools(&mut self) -> Vec<ToolSpec> {
        if self.0.is_empty() {
            return Vec::new();
        }
        let names: Vec<Value> = self.0.entries().iter().map(|e| json!(e.name)).collect();
        vec![
            ToolSpec {
                name: LOAD_SKILL.into(),
                description: "Load a skill's full instructions (the skills are listed in the system prompt). Call it before work a skill covers."
                    .into(),
                input_schema: json!({"type": "object", "properties": {"name": {"type": "string", "enum": names, "description": "The skill's name"}}, "required": ["name"], "additionalProperties": false}),
                strict: false,
            },
            ToolSpec {
                name: READ_SKILL_FILE.into(),
                description: "Read a file that comes with a skill (load_skill lists them).".into(),
                input_schema: json!({"type": "object", "properties": {"name": {"type": "string", "description": "The skill's name"}, "path": {"type": "string", "description": "The file, as load_skill lists it"}}, "required": ["name", "path"], "additionalProperties": false}),
                strict: false,
            },
        ]
    }

    fn call(&mut self, call: &ToolCall, _cancel: &Cancel) -> ToolOutput {
        let s = |k: &str| call.input.get(k).and_then(Value::as_str).unwrap_or_default();
        let r = match call.name.as_str() {
            LOAD_SKILL => self.0.load(s("name")),
            READ_SKILL_FILE => self.0.read_file(s("name"), s("path")),
            other => Err(format!("There is no tool named `{other}`.")),
        };
        match r {
            Ok(t) => ToolOutput::ok(t),
            Err(e) => ToolOutput::error(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> PathBuf {
        let d = std::env::temp_dir().join(format!("ashlar-skills-{}-{}", std::process::id(), crate::history::new_id()));
        let _ = std::fs::create_dir_all(d.join("lower-thirds/templates"));
        let _ = std::fs::write(
            d.join("lower-thirds/SKILL.md"),
            "---\nname: lower-thirds\ndescription: Name straps that slide in.\n---\nUse templates/strap.json.",
        );
        let _ = std::fs::write(d.join("lower-thirds/templates/strap.json"), "{\"h\": 120}");
        d
    }

    fn catalog(d: &Path) -> Skills {
        let mut s = Skills::new();
        let lib = ashlar_docs::Library::from_files([
            ("skills/titles.md", "---\ntitle: Titles\ndescription: Make a title card.\n---\nSee [motion](../reference/motion.md)."),
            ("skills/lower-thirds.md", "---\ntitle: LT\ndescription: Built-in straps.\n---\nbuilt-in body"),
            ("skills/nodesc.md", "---\ntitle: x\n---\nskipped"),
            ("skills/kit/SKILL.md", "---\nname: kit\ndescription: The kit.\n---\nkit body"),
        ]);
        assert_eq!(s.add_library(&lib, "skills", "built-in"), 3);
        assert_eq!(s.add_dir(d, "", "yours"), 1, "the person's replaces the built-in one");
        let fetched: Fetch = Arc::new(|| Ok("# Mixing\nKeep peaks under -1 dB.".to_string()));
        s.add(SkillEntry {
            name: "earshot:mixing".into(),
            description: "Use when balancing levels.".into(),
            source: "earshot".into(),
            content: SkillContent::Fetch(fetched),
        });
        s
    }

    #[test]
    fn listed_by_name_and_description_loaded_on_demand() {
        let d = dir();
        let s = catalog(&d);
        let names: Vec<&str> = s.entries().iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["earshot:mixing", "kit", "lower-thirds", "titles"]);
        let p = s.prompt_section();
        assert!(p.contains("- `lower-thirds`: Name straps that slide in.") && p.contains("- `earshot:mixing`: Use when balancing levels."), "{p}");
        assert!(!p.contains("Use templates"), "bodies stay out of the prompt");
        assert!(s.load("titles").unwrap().contains("[motion](docs://reference/motion.md)"));
        let lt = s.load("lower-thirds").unwrap();
        assert!(
            lt.contains("Use templates/strap.json.") && lt.contains("Files in this skill (read one with `read_skill_file`): templates/strap.json"),
            "{lt}"
        );
        assert_eq!(s.read_file("lower-thirds", "templates/strap.json").unwrap(), "{\"h\": 120}");
        assert!(s.read_file("lower-thirds", "../../etc/passwd").is_err());
        assert!(s.read_file("titles", "x").is_err());
        assert!(s.load("earshot:mixing").unwrap().contains("-1 dB"));
        assert!(s.load("nope").is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn tools_commands_and_picking() {
        let d = dir();
        let s = Arc::new(catalog(&d));
        let mut t = s.tools();
        let specs = t.tools();
        assert_eq!(specs.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(), [LOAD_SKILL, READ_SKILL_FILE]);
        let call = |name: &str, input: Value| ToolCall { id: "1".into(), name: name.into(), input, invalid_input: None };
        let out = t.call(&call(LOAD_SKILL, json!({"name": "kit"})), &Cancel::new());
        assert!(!out.is_error && out.content.contains("kit body"));
        assert!(t.call(&call(LOAD_SKILL, json!({"name": "zzz"})), &Cancel::new()).is_error);
        let cmds = s.slash_commands();
        assert!(cmds.iter().any(|c| c.name == "earshot:mixing" && c.action == SlashAction::Skill { name: "earshot:mixing".into() }));
        let mut reg = crate::slash::SlashRegistry::with_builtins();
        for c in cmds {
            assert!(reg.add(c));
        }
        assert!(reg.get("lower-thirds").is_some());
        let m = s.picked("lower-thirds").unwrap();
        assert_eq!((m.kind.as_str(), m.id.as_str()), ("skill", "lower-thirds"));
        assert!(m.detail.contains("Use templates/strap.json."));
        assert!(s.picked("earshot:mixing").unwrap().detail.contains("Load it with `load_skill`"), "fetched ones aren't fetched on the UI thread");
        assert!(Arc::new(Skills::new()).tools().tools().is_empty(), "no skills, no tools");
        let _ = std::fs::remove_dir_all(&d);
    }
}
