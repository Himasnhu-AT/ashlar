//! A documentation library for agents: folders are categories (nested folders are
//! subcategories), markdown files are documents, and every page is served verbatim.
//!
//! - `_category.md` in a folder describes it (frontmatter `title`, `description`; the body is the
//!   overview). It is not listed as a document. The root one says what the library is.
//! - A document's frontmatter has `title`, `description` (when to read it), and optionally
//!   `order` (lower first) and `tags`.
//! - [`embed`] (build scripts) writes a `(path, text)` table of a folder for `include!`, so a page
//!   joins by being added to the folder. [`Library::overlay_dir`] adds a folder at run time on
//!   top, where the same path replaces the built-in page.

use std::fmt::Write as _;
use std::path::Path;

/// The file in a folder that describes its category.
pub const CATEGORY_FILE: &str = "_category.md";
/// Folders deeper than this are not read.
pub const MAX_DEPTH: usize = 8;
/// A run-time folder contributes at most this many files.
pub const MAX_FILES: usize = 2000;
/// A run-time file larger than this is skipped.
pub const MAX_FILE_BYTES: u64 = 1024 * 1024;

/// Where a page came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Built into the program.
    BuiltIn,
    /// A folder added at run time ([`Library::overlay_dir`]).
    User,
}

impl Source {
    /// `built-in` or `yours`, as listings show it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BuiltIn => "built-in",
            Self::User => "yours",
        }
    }
}

/// One document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Doc {
    /// Path from the library root, e.g. `skills/review.md`.
    pub path: String,
    pub title: String,
    pub description: String,
    /// The folder it is in (`""` for the root), e.g. `reference/motion`.
    pub category: String,
    pub order: Option<i64>,
    pub tags: Vec<String>,
    pub source: Source,
    /// The file, verbatim (frontmatter included).
    pub text: String,
}

/// One category (folder). A folder without `_category.md` still is one, titled after its name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Category {
    /// Path from the library root (`""` for the root), e.g. `reference/motion`.
    pub path: String,
    pub title: String,
    pub description: String,
    /// The body of `_category.md` (after the frontmatter), trimmed.
    pub overview: String,
    /// `_category.md` verbatim, when there is one.
    pub text: Option<String>,
    pub source: Source,
}

/// A category with what is under it, for [`Library::tree`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    pub path: String,
    pub title: String,
    pub description: String,
    /// Documents directly in this category.
    pub docs: usize,
    /// Documents in this category and all below it.
    pub total: usize,
    pub children: Vec<Node>,
}

/// Documents and categories, sorted by path.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Library {
    docs: Vec<Doc>,
    categories: Vec<Category>,
}

impl Library {
    pub fn new() -> Self {
        Self::default()
    }

    /// A library of built-in files (`(path, text)`, as [`embed`] writes them).
    pub fn from_files<'a>(files: impl IntoIterator<Item = (&'a str, &'a str)>) -> Self {
        let mut lib = Self::new();
        for (path, text) in files {
            lib.insert(path, text, Source::BuiltIn);
        }
        lib
    }

    /// Add or replace one file: `_category.md` describes its folder, any other `.md` is a document.
    pub fn insert(&mut self, path: &str, text: &str, source: Source) {
        let path = normalize(path);
        let (dir, name) = split(&path);
        let front = Frontmatter::parse(text);
        if name == CATEGORY_FILE {
            let cat = Category {
                path: dir.to_string(),
                title: front.title.clone().unwrap_or_else(|| default_title(dir)),
                description: front.description.clone().unwrap_or_default(),
                overview: front.body.trim().to_string(),
                text: Some(text.to_string()),
                source,
            };
            self.categories.retain(|c| c.path != cat.path);
            self.categories.push(cat);
            self.categories.sort_by(|a, b| a.path.cmp(&b.path));
            return;
        }
        let stem = name.strip_suffix(".md").unwrap_or(name);
        let doc = Doc {
            title: front.title.clone().unwrap_or_else(|| first_heading(front.body).unwrap_or_else(|| humanize(stem))),
            description: front.description.clone().unwrap_or_default(),
            category: dir.to_string(),
            order: front.order,
            tags: front.tags.clone(),
            source,
            text: text.to_string(),
            path,
        };
        self.docs.retain(|d| d.path != doc.path);
        self.docs.push(doc);
        self.docs.sort_by(|a, b| a.path.cmp(&b.path));
    }

    /// Add the markdown files under `dir` on top (source [`Source::User`]; the same path replaces
    /// a page). Symbolic links, dotfiles and non-markdown files are skipped, as are files over
    /// [`MAX_FILE_BYTES`], folders deeper than [`MAX_DEPTH`] and files past [`MAX_FILES`]. A
    /// missing folder adds nothing.
    pub fn overlay_dir(mut self, dir: &Path) -> Self {
        let mut files = Vec::new();
        walk(dir, "", 0, &mut files);
        for (rel, file) in files.into_iter().take(MAX_FILES) {
            let small = std::fs::metadata(&file).is_ok_and(|m| m.len() <= MAX_FILE_BYTES);
            if let (true, Ok(text)) = (small, std::fs::read_to_string(&file)) {
                self.insert(&rel, &text, Source::User);
            }
        }
        self
    }

    pub fn is_empty(&self) -> bool {
        self.docs.is_empty() && self.categories.is_empty()
    }

    /// Every document, sorted by path.
    pub fn docs(&self) -> &[Doc] {
        &self.docs
    }

    /// The categories that have a `_category.md`, sorted by path.
    pub fn described(&self) -> &[Category] {
        &self.categories
    }

    /// The root overview (the body of the root `_category.md`).
    pub fn overview(&self) -> Option<&str> {
        self.categories.iter().find(|c| c.path.is_empty()).map(|c| c.overview.as_str()).filter(|o| !o.is_empty())
    }

    /// A category by path (`""` is the root): described by its `_category.md`, or implied by the
    /// documents under it.
    pub fn category(&self, path: &str) -> Option<Category> {
        let path = normalize(path);
        if let Some(c) = self.categories.iter().find(|c| c.path == path) {
            return Some(c.clone());
        }
        let known = path.is_empty() || self.all_category_paths().contains(&path);
        known.then(|| Category {
            title: default_title(&path),
            path,
            description: String::new(),
            overview: String::new(),
            text: None,
            source: Source::BuiltIn,
        })
    }

    /// The category tree under `path` (`""` for all of it); `None` for an unknown category.
    pub fn tree(&self, path: &str) -> Option<Node> {
        let cat = self.category(path)?;
        Some(self.node(cat))
    }

    /// Documents in `category` (all when `None`; with `recursive`, also those in its
    /// subcategories) that contain `query` (case-insensitive, in path, title, description, tags
    /// or text), ordered by category, then `order`, then path.
    pub fn list(&self, category: Option<&str>, query: Option<&str>, recursive: bool) -> Vec<&Doc> {
        let cat = category.map(normalize);
        let q = query.map(str::to_lowercase).filter(|q| !q.is_empty());
        let mut out: Vec<&Doc> = self
            .docs
            .iter()
            .filter(|d| match &cat {
                None => true,
                Some(c) if c.is_empty() => recursive || d.category.is_empty(),
                Some(c) => d.category == *c || (recursive && d.category.starts_with(&format!("{c}/"))),
            })
            .filter(|d| q.as_deref().is_none_or(|q| d.matches(q)))
            .collect();
        out.sort_by(|a, b| a.category.cmp(&b.category).then(a.order.unwrap_or(i64::MAX).cmp(&b.order.unwrap_or(i64::MAX))).then(a.path.cmp(&b.path)));
        out
    }

    /// The document at exactly `path` (or a category's `_category.md`); an unknown path is an
    /// error that names the closest pages.
    pub fn read(&self, path: &str) -> Result<&str, String> {
        let p = normalize(path.trim().trim_start_matches("docs://"));
        if let Some(d) = self.docs.iter().find(|d| d.path == p) {
            return Ok(&d.text);
        }
        let (dir, name) = split(&p);
        if name == CATEGORY_FILE
            && let Some(text) = self.categories.iter().find(|c| c.path == dir).and_then(|c| c.text.as_deref())
        {
            return Ok(text);
        }
        if self.category(&p).is_some() {
            return Err(format!("`{p}` is a category, not a document: list it to see its documents"));
        }
        let close = self.close_matches(&p);
        if close.is_empty() {
            Err(format!("no document `{p}`"))
        } else {
            Err(format!("no document `{p}`; did you mean {}?", close.iter().map(|c| format!("`{c}`")).collect::<Vec<_>>().join(", ")))
        }
    }

    /// Up to five document paths that share the file name or contain the name's stem.
    pub fn close_matches(&self, path: &str) -> Vec<&str> {
        let (_, name) = split(path);
        let stem = name.strip_suffix(".md").unwrap_or(name).to_lowercase();
        if stem.is_empty() {
            return Vec::new();
        }
        let same: Vec<&str> = self.docs.iter().filter(|d| split(&d.path).1.eq_ignore_ascii_case(name)).map(|d| d.path.as_str()).collect();
        let near = self.docs.iter().filter(|d| d.path.to_lowercase().contains(&stem)).map(|d| d.path.as_str());
        let mut out = same;
        for p in near {
            if !out.contains(&p) {
                out.push(p);
            }
        }
        out.truncate(5);
        out
    }

    fn all_category_paths(&self) -> Vec<String> {
        let mut paths: Vec<String> = Vec::new();
        let dirs = self.docs.iter().map(|d| d.category.as_str()).chain(self.categories.iter().map(|c| c.path.as_str()));
        for dir in dirs {
            let mut acc = String::new();
            for part in dir.split('/').filter(|p| !p.is_empty()) {
                if !acc.is_empty() {
                    acc.push('/');
                }
                acc.push_str(part);
                if !paths.contains(&acc) {
                    paths.push(acc.clone());
                }
            }
        }
        paths.sort();
        paths
    }

    fn node(&self, cat: Category) -> Node {
        let prefix = if cat.path.is_empty() { String::new() } else { format!("{}/", cat.path) };
        let children: Vec<Node> = self
            .all_category_paths()
            .into_iter()
            .filter(|p| p.strip_prefix(&prefix).is_some_and(|rest| !rest.is_empty() && !rest.contains('/')))
            .filter_map(|p| self.category(&p))
            .map(|c| self.node(c))
            .collect();
        let docs = self.docs.iter().filter(|d| d.category == cat.path).count();
        let total = docs + children.iter().map(|c| c.total).sum::<usize>();
        Node { path: cat.path, title: cat.title, description: cat.description, docs, total, children }
    }
}

impl Doc {
    fn matches(&self, q: &str) -> bool {
        [&self.path, &self.title, &self.description, &self.text].iter().any(|s| s.to_lowercase().contains(q))
            || self.tags.iter().any(|t| t.to_lowercase().contains(q))
    }
}

/// The frontmatter fields this library reads, and the text after it.
#[derive(Debug, Default)]
pub struct Frontmatter<'a> {
    pub title: Option<String>,
    pub description: Option<String>,
    pub order: Option<i64>,
    pub tags: Vec<String>,
    pub body: &'a str,
}

impl<'a> Frontmatter<'a> {
    /// Read a leading `---` … `---` block of `key: value` lines (values may be quoted; `tags` is
    /// `[a, b]` or `a, b`). Text without one is all body.
    pub fn parse(text: &'a str) -> Self {
        let mut out = Self { body: text, ..Self::default() };
        let rest = text.strip_prefix('\u{feff}').unwrap_or(text);
        let Some(after_open) = rest.strip_prefix("---\n").or_else(|| rest.strip_prefix("---\r\n")) else { return out };
        let mut offset = 0;
        let mut closed = None;
        for line in after_open.split_inclusive('\n') {
            if line.trim_end() == "---" || line.trim_end() == "..." {
                closed = Some(offset + line.len());
                break;
            }
            if let Some((key, value)) = line.split_once(':') {
                let value = unquote(value.trim());
                match key.trim() {
                    "title" => out.title = Some(value).filter(|v| !v.is_empty()),
                    "description" => out.description = Some(value).filter(|v| !v.is_empty()),
                    "order" => out.order = value.parse().ok(),
                    "tags" => {
                        out.tags = value
                            .trim_start_matches('[')
                            .trim_end_matches(']')
                            .split(',')
                            .map(|t| unquote(t.trim()))
                            .filter(|t| !t.is_empty())
                            .collect()
                    }
                    _ => {}
                }
            }
            offset += line.len();
        }
        if let Some(end) = closed {
            out.body = after_open.get(end..).unwrap_or("");
        } else {
            out = Self { body: text, ..Self::default() };
        }
        out
    }
}

/// Write the markdown files under `dir` as a Rust table for `include!`:
/// `pub static FILES: &[(&str, &str)] = &[("skills/review.md", include_str!("…")), …];`, sorted
/// by path, and print `cargo:rerun-if-changed` for every folder and file so an added, removed or
/// edited page rebuilds the crate. For build scripts. Returns how many files were written.
pub fn embed(dir: &Path, out_file: &Path) -> std::io::Result<usize> {
    let mut files = Vec::new();
    walk(dir, "", 0, &mut files);
    files.sort();
    println!("cargo:rerun-if-changed={}", dir.display());
    let mut dirs = Vec::new();
    walk_dirs(dir, 0, &mut dirs);
    for d in &dirs {
        println!("cargo:rerun-if-changed={}", d.display());
    }
    let mut src =
        String::from("/// Every documentation file, `(path from the docs root, text)`, sorted by path.\npub static FILES: &[(&str, &str)] = &[\n");
    for (rel, file) in &files {
        println!("cargo:rerun-if-changed={}", file.display());
        let abs = std::fs::canonicalize(file)?;
        let _ = writeln!(src, "    ({rel:?}, include_str!({:?})),", abs.display().to_string());
    }
    src.push_str("];\n");
    std::fs::write(out_file, src)?;
    Ok(files.len())
}

/// Markdown files under `dir` as `(path from the root, file)`; links, dotfiles and other files
/// skipped.
fn walk(dir: &Path, rel: &str, depth: usize, out: &mut Vec<(String, std::path::PathBuf)>) {
    if depth > MAX_DEPTH || out.len() >= MAX_FILES {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        // `file_type` does not follow links, so a link is neither a file nor a folder here
        let Ok(kind) = entry.file_type() else { continue };
        if name.starts_with('.') || kind.is_symlink() {
            continue;
        }
        let child = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
        if kind.is_dir() {
            walk(&entry.path(), &child, depth + 1, out);
        } else if kind.is_file() && name.ends_with(".md") {
            out.push((child, entry.path()));
        }
    }
}

fn walk_dirs(dir: &Path, depth: usize, out: &mut Vec<std::path::PathBuf>) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let hidden = entry.file_name().to_string_lossy().starts_with('.');
        if !hidden && entry.file_type().is_ok_and(|k| k.is_dir()) {
            out.push(entry.path());
            walk_dirs(&entry.path(), depth + 1, out);
        }
    }
}

/// `/a//b/` → `a/b`, backslashes as slashes.
fn normalize(path: &str) -> String {
    path.replace('\\', "/").split('/').filter(|p| !p.is_empty() && *p != ".").collect::<Vec<_>>().join("/")
}

/// `(folder, file name)` of a normalized path.
fn split(path: &str) -> (&str, &str) {
    path.rsplit_once('/').unwrap_or(("", path))
}

fn unquote(v: &str) -> String {
    let inner = v.strip_prefix('"').and_then(|s| s.strip_suffix('"')).or_else(|| v.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')));
    match inner {
        Some(s) if v.starts_with('"') => s.replace("\\\"", "\"").replace("\\\\", "\\"),
        Some(s) => s.replace("''", "'"),
        None => v.to_string(),
    }
}

fn first_heading(body: &str) -> Option<String> {
    body.lines().find_map(|l| l.strip_prefix("# ")).map(|t| t.trim().to_string()).filter(|t| !t.is_empty())
}

fn default_title(dir: &str) -> String {
    if dir.is_empty() { "Documentation".to_string() } else { humanize(split(dir).1) }
}

/// `ui-demo_recipes` → `Ui demo recipes`.
fn humanize(name: &str) -> String {
    let words: Vec<&str> = name.split(['-', '_']).filter(|w| !w.is_empty()).collect();
    let joined = words.join(" ");
    let mut chars = joined.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => name.to_string(),
    }
}
