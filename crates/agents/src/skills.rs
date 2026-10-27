//! Agent skills (`SKILL.md`: `name` and `description` frontmatter, then instructions) made from a
//! category of an app's docs library, so the skills an agent loads are the pages the installed
//! version of the app ships; and skills read back from disk ([`parse`], [`load_dir`]): a folder
//! of skill folders, each a `SKILL.md` and the files it refers to.

use std::path::{Path, PathBuf};

use ashlar_docs::{Doc, Frontmatter, Library};

/// Longest skill name agents accept.
pub const MAX_NAME: usize = 64;
/// Longest description agents accept.
pub const MAX_DESCRIPTION: usize = 1024;

/// One skill: a folder `<name>/SKILL.md`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    /// Lowercase letters, digits and hyphens ([`name`]); also the folder name.
    pub name: String,
    /// When an agent should use it (at most [`MAX_DESCRIPTION`] characters).
    pub description: String,
    /// The instructions (markdown, no frontmatter).
    pub body: String,
}

impl Skill {
    pub fn new(name: &str, description: &str, body: &str) -> Self {
        Self { name: self::name(name), description: one_line(description, MAX_DESCRIPTION), body: body.trim().to_string() }
    }

    /// The `SKILL.md` file. `marker` (one line, an HTML comment) goes right after the frontmatter
    /// so an uninstall can tell the file is ours.
    pub fn to_file(&self, marker: &str) -> String {
        format!("---\nname: {}\ndescription: {}\n---\n{marker}\n\n{}\n", self.name, yaml_scalar(&self.description), self.body)
    }
}

/// A skill read from a `SKILL.md` (or any markdown page with the same frontmatter). The name is
/// the frontmatter's `name`, else `fallback_name` (the folder's or file's name), made a valid
/// [`name`]. `None` without a description (no one could tell when to use it) or a name.
pub fn parse(text: &str, fallback_name: &str) -> Option<Skill> {
    let fm = Frontmatter::parse(text);
    let description = fm.description.filter(|d| !d.trim().is_empty())?;
    let declared = front_field(text, "name");
    let skill_name = name(declared.as_deref().filter(|n| !n.trim().is_empty()).unwrap_or(fallback_name));
    if skill_name.is_empty() {
        return None;
    }
    Some(Skill::new(&skill_name, &description, fm.body))
}

/// One `key: value` of a leading frontmatter block (quotes removed).
fn front_field(text: &str, key: &str) -> Option<String> {
    let rest = text.strip_prefix('\u{feff}').unwrap_or(text);
    let after = rest.strip_prefix("---\n").or_else(|| rest.strip_prefix("---\r\n"))?;
    for line in after.lines() {
        if line.trim_end() == "---" {
            break;
        }
        if let Some((k, v)) = line.split_once(':')
            && k.trim() == key
        {
            return Some(v.trim().trim_matches('"').trim_matches('\'').to_string());
        }
    }
    None
}

/// The file every skill folder has.
pub const SKILL_FILE: &str = "SKILL.md";
/// The most skills read from one folder.
pub const MAX_SKILLS: usize = 500;
/// The largest `SKILL.md` read.
pub const MAX_SKILL_BYTES: u64 = 512 * 1024;
/// The most extra files listed for one skill.
pub const MAX_FILES: usize = 100;

/// A skill on disk: its `SKILL.md` and the other files in its folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillDir {
    pub skill: Skill,
    /// The skill's folder.
    pub dir: PathBuf,
    /// The other files, relative to `dir` (`/`-separated, sorted), at most [`MAX_FILES`], two
    /// folders deep; dotfiles left out.
    pub files: Vec<String>,
}

/// The skills under `root`: every `<root>/<folder>/SKILL.md` (named by its frontmatter or the
/// folder), and every `<root>/<page>.md` with skill frontmatter (a skill of one page). Symbolic
/// links, dotfiles, oversized and unreadable files are skipped; a missing `root` is no skills.
/// Sorted by name; a name seen twice keeps the first.
pub fn load_dir(root: &Path) -> Vec<SkillDir> {
    let Ok(entries) = std::fs::read_dir(root) else { return Vec::new() };
    let mut paths: Vec<PathBuf> = entries.filter_map(Result::ok).map(|e| e.path()).collect();
    paths.sort();
    let mut out: Vec<SkillDir> = Vec::new();
    for p in paths.into_iter().take(MAX_SKILLS.saturating_mul(2)) {
        let Some(file_name) = p.file_name().and_then(|n| n.to_str()).map(str::to_string) else { continue };
        if file_name.starts_with('.') || file_name.starts_with('_') || p.symlink_metadata().map(|m| m.file_type().is_symlink()).unwrap_or(true) {
            continue;
        }
        let found = if p.is_dir() {
            read_small(&p.join(SKILL_FILE)).and_then(|t| parse(&t, &file_name)).map(|skill| SkillDir { files: list_files(&p), skill, dir: p.clone() })
        } else if file_name.ends_with(".md") {
            let stem = file_name.trim_end_matches(".md");
            read_small(&p).and_then(|t| parse(&t, stem)).map(|skill| SkillDir { skill, dir: root.to_path_buf(), files: Vec::new() })
        } else {
            None
        };
        if let Some(s) = found
            && !out.iter().any(|o| o.skill.name == s.skill.name)
        {
            out.push(s);
        }
        if out.len() >= MAX_SKILLS {
            break;
        }
    }
    out.sort_by(|a, b| a.skill.name.cmp(&b.skill.name));
    out
}

fn read_small(p: &Path) -> Option<String> {
    let m = std::fs::symlink_metadata(p).ok()?;
    if !m.is_file() || m.len() > MAX_SKILL_BYTES {
        return None;
    }
    std::fs::read_to_string(p).ok()
}

/// The files of a skill folder besides `SKILL.md`.
fn list_files(dir: &Path) -> Vec<String> {
    fn walk(base: &Path, dir: &Path, depth: u32, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        let mut paths: Vec<PathBuf> = entries.filter_map(Result::ok).map(|e| e.path()).collect();
        paths.sort();
        for p in paths {
            if out.len() >= MAX_FILES {
                return;
            }
            let Some(n) = p.file_name().and_then(|n| n.to_str()) else { continue };
            let Ok(m) = p.symlink_metadata() else { continue };
            if n.starts_with('.') || m.file_type().is_symlink() {
                continue;
            }
            if m.is_dir() {
                if depth < 2 {
                    walk(base, &p, depth + 1, out);
                }
            } else if let Ok(rel) = p.strip_prefix(base) {
                let rel = rel.components().filter_map(|c| c.as_os_str().to_str()).collect::<Vec<_>>().join("/");
                if rel != SKILL_FILE {
                    out.push(rel);
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, 0, &mut out);
    out
}

/// `s` as a skill name: lowercase ASCII letters, digits and single hyphens, at most [`MAX_NAME`]
/// characters, never starting or ending with a hyphen.
pub fn name(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let mut out: String = out.chars().take(MAX_NAME).collect();
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// `s` on one line (runs of whitespace become one space), cut to `max` characters.
fn one_line(s: &str, max: usize) -> String {
    let joined = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if joined.chars().count() <= max {
        return joined;
    }
    let mut cut: String = joined.chars().take(max.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

/// A YAML plain scalar when that reads back as the same text, else a double-quoted one.
fn yaml_scalar(s: &str) -> String {
    let risky = s.is_empty()
        || s.contains(": ")
        || s.contains(" #")
        || s.ends_with(':')
        || s.starts_with(|c: char| "-?:,[]{}#&*!|>'\"%@`".contains(c) || c.is_whitespace());
    if risky { serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into()) } else { s.to_string() }
}

/// One skill per document in `category` of `lib` (and its subcategories), named
/// `<prefix>-<path in the category>` (`skills/review.md` with prefix `app` is `app-review`). The
/// body is the page after its frontmatter, preceded by `header(doc)` (say where it comes from and
/// how to read the rest), with every relative link to another page rewritten to its `docs://`
/// address, which the app's docs tools read. Pages without a description are skipped (an agent
/// could never tell when to use them).
pub fn from_category(lib: &Library, category: &str, prefix: &str, header: impl Fn(&Doc) -> String) -> Vec<Skill> {
    let cat = category.trim_matches('/');
    let mut out: Vec<Skill> = Vec::new();
    for doc in lib.list(Some(cat), None, true) {
        if doc.description.trim().is_empty() {
            continue;
        }
        let rel = doc.path.strip_prefix(cat).unwrap_or(&doc.path).trim_start_matches('/');
        let stem = rel.strip_suffix(".md").unwrap_or(rel);
        let skill_name = name(&format!("{prefix}-{stem}"));
        if skill_name.is_empty() || out.iter().any(|s| s.name == skill_name) {
            continue;
        }
        let body = rewrite_links(Frontmatter::parse(&doc.text).body, &doc.category);
        let head = header(doc);
        let body = if head.trim().is_empty() { body } else { format!("{}\n\n{}", head.trim_end(), body.trim_start()) };
        out.push(Skill::new(&skill_name, &doc.description, &body));
    }
    out
}

/// Markdown links to other pages (`](../reference/a.md#part)`), relative to the folder `dir` of
/// the library, as `docs://` addresses (`](docs://reference/a.md#part)`). Absolute links, other
/// schemes and in-page anchors are left alone.
pub fn rewrite_links(body: &str, dir: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut rest = body;
    while let Some(at) = rest.find("](") {
        let (head, tail) = rest.split_at(at + 2);
        out.push_str(head);
        let Some(close) = tail.find(')') else {
            rest = tail;
            continue;
        };
        let target = tail.get(..close).unwrap_or("");
        let (path, frag) = target.split_once('#').map_or((target, None), |(p, f)| (p, Some(f)));
        let local = !path.is_empty() && !path.contains("://") && !path.starts_with('/') && !path.starts_with("mailto:") && !path.contains(' ');
        match resolve(dir, path).filter(|p| local && p.ends_with(".md")) {
            Some(resolved) => {
                out.push_str("docs://");
                out.push_str(&resolved);
                if let Some(f) = frag {
                    out.push('#');
                    out.push_str(f);
                }
            }
            None => out.push_str(target),
        }
        rest = tail.get(close..).unwrap_or("");
    }
    out.push_str(rest);
    out
}

/// `rel` relative to the folder `dir`, as a path from the library root; `None` when it climbs out.
fn resolve(dir: &str, rel: &str) -> Option<String> {
    let mut parts: Vec<&str> = dir.split('/').filter(|p| !p.is_empty()).collect();
    for p in rel.split('/') {
        match p {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            p => parts.push(p),
        }
    }
    Some(parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lib() -> Library {
        Library::from_files([
            ("_category.md", "---\ntitle: Docs\ndescription: All of it.\n---\nRoot."),
            ("skills/_category.md", "---\ntitle: Skills\ndescription: House rules.\n---\n"),
            (
                "skills/review.md",
                "---\ntitle: Review\ndescription: Before you say a cut is done: check it.\n---\n# Review\n\nSee [intake](intake.md) and [motion](../reference/motion.md#timing), [site](https://example.com/a.md), [top](#review).\n",
            ),
            ("skills/intake.md", "---\ntitle: Intake\ndescription: First, every job.\norder: 1\n---\nAsk.\n"),
            ("skills/deep/Two Words.md", "---\ntitle: Two\ndescription: Nested.\n---\nNested body.\n"),
            ("skills/nodesc.md", "---\ntitle: No description\n---\nskipped\n"),
            ("reference/motion.md", "---\ntitle: Motion\ndescription: Timing.\n---\nx"),
        ])
    }

    #[test]
    fn one_skill_per_page_with_links_as_docs_addresses() {
        let skills = from_category(&lib(), "skills", "app", |d| format!("From `{}`.", d.path));
        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["app-intake", "app-review", "app-deep-two-words"]);
        let review = skills.iter().find(|s| s.name == "app-review").unwrap();
        assert_eq!(review.description, "Before you say a cut is done: check it.");
        assert!(review.body.starts_with("From `skills/review.md`.\n\n# Review"), "{}", review.body);
        assert!(review.body.contains("[intake](docs://skills/intake.md)"));
        assert!(review.body.contains("[motion](docs://reference/motion.md#timing)"));
        assert!(review.body.contains("[site](https://example.com/a.md)") && review.body.contains("[top](#review)"));
        let file = review.to_file("<!-- ours -->");
        assert!(
            file.starts_with("---\nname: app-review\ndescription: \"Before you say a cut is done: check it.\"\n---\n<!-- ours -->\n\nFrom"),
            "{file}"
        );
    }

    #[test]
    fn names_follow_the_skill_rules() {
        assert_eq!(name("App_Review  Two!"), "app-review-two");
        assert_eq!(name("--x--"), "x");
        assert_eq!(name(&"a".repeat(80)).len(), MAX_NAME);
        assert_eq!(name(&format!("{}-b", "a".repeat(63))), "a".repeat(63));
        assert_eq!(Skill::new("n", &"word ".repeat(400), "b").description.chars().count(), MAX_DESCRIPTION);
    }

    #[test]
    fn links_that_climb_out_of_the_library_stay() {
        assert_eq!(rewrite_links("[a](../../x.md)", "skills"), "[a](../../x.md)");
        assert_eq!(rewrite_links("[a](./b/c.md) [b](c.png) (x) ](", "skills"), "[a](docs://skills/b/c.md) [b](c.png) (x) ](");
    }

    #[test]
    fn skills_read_back_from_folders_and_pages() {
        let root = tempfile::tempdir().unwrap();
        let r = root.path();
        std::fs::create_dir_all(r.join("Lower Thirds/templates")).unwrap();
        std::fs::write(
            r.join("Lower Thirds/SKILL.md"),
            "---\nname: lower-thirds\ndescription: Name straps that slide in.\n---\n# Lower thirds\nUse templates/a.json.",
        )
        .unwrap();
        std::fs::write(r.join("Lower Thirds/templates/a.json"), "{}").unwrap();
        std::fs::write(r.join("Lower Thirds/.hidden"), "x").unwrap();
        std::fs::create_dir_all(r.join("nodesc")).unwrap();
        std::fs::write(r.join("nodesc/SKILL.md"), "---\nname: x\n---\nbody").unwrap();
        std::fs::write(r.join("Logo Reveal.md"), "---\ndescription: Reveal a logo.\n---\nSteps.").unwrap();
        std::fs::write(r.join("notes.txt"), "ignored").unwrap();
        let skills = load_dir(r);
        let names: Vec<&str> = skills.iter().map(|s| s.skill.name.as_str()).collect();
        assert_eq!(names, ["logo-reveal", "lower-thirds"]);
        let lt = &skills[1];
        assert_eq!(lt.files, ["templates/a.json"]);
        assert_eq!(lt.skill.body, "# Lower thirds\nUse templates/a.json.");
        assert!(lt.dir.ends_with("Lower Thirds"));
        assert!(load_dir(&r.join("missing")).is_empty());
        assert_eq!(parse("no frontmatter", "x"), None);
        assert_eq!(parse("---\ndescription: d\n---\nb", "My Skill").map(|s| s.name), Some("my-skill".into()));
    }

    #[test]
    fn plain_descriptions_stay_plain() {
        assert_eq!(yaml_scalar("Use when cutting."), "Use when cutting.");
        assert_eq!(yaml_scalar("Note: x"), "\"Note: x\"");
        assert_eq!(yaml_scalar("#tag"), "\"#tag\"");
    }
}
