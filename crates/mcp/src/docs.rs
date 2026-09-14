//! Documentation and AI skills for agents, served as MCP resources (`docs://<path>`), the
//! `list_docs` / `read_doc` tools and the CLI's `docs` verb.
//!
//! Apps embed their pages in the binary ([`Doc::markdown`] over `include_str!`), so a shipped
//! build needs no files beside it. [`load_docs_from_dir`] is for an explicit directory only.

use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

const MAX_DOC_BYTES: usize = 10 * 1024 * 1024; // 10 MiB limit per document
const SAMPLE_BYTES: usize = 65536; // 64 KiB sample for metadata extraction
const MAX_WALK_DEPTH: usize = 8;

/// A markdown documentation file or AI skill resource.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Doc {
    /// MCP resource URI, e.g. `docs://video/skills/launch-film.md`.
    pub uri: String,
    /// Relative path within the documentation root, e.g. `video/skills/launch-film.md`.
    pub path: String,
    /// Short identifier or file stem, e.g. `launch-film`.
    pub name: String,
    /// Document title extracted from frontmatter or the first `# Heading`.
    pub title: String,
    /// Short description or summary.
    pub description: String,
    /// Category or section, e.g. `skills`, `styles`, `reference`, `motion`.
    pub section: String,
    /// Embedded content if provided in-memory.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// On-disk path if backed by a file.
    #[serde(skip)]
    pub source_path: Option<PathBuf>,
}

impl Doc {
    /// Create a document from in-memory content.
    pub fn new(path: impl Into<String>, title: impl Into<String>, description: impl Into<String>, content: impl Into<String>) -> Self {
        let path = path.into();
        let name = Path::new(&path).file_stem().and_then(|s| s.to_str()).unwrap_or(&path).to_string();
        let section = derive_section(&path);
        let uri = format!("docs://{path}");
        Self { uri, path, name, title: title.into(), description: description.into(), section, content: Some(content.into()), source_path: None }
    }

    /// A document from embedded markdown, its title and description read from the text
    /// (frontmatter `title:` / `description:`, else the first heading and paragraph).
    pub fn markdown(path: impl Into<String>, text: impl Into<String>) -> Self {
        let path = path.into();
        let text = text.into();
        let name = Path::new(&path).file_stem().and_then(|s| s.to_str()).unwrap_or(&path).to_string();
        let (title, description) = extract_metadata(&text, &name);
        Self { title, description, ..Self::new(path, "", "", text) }
    }

    /// Whether the document passes a `list_docs` / `docs` filter: `section` is a case-insensitive
    /// substring of its section, `query` of its path, title, description or embedded text.
    pub fn matches(&self, query: Option<&str>, section: Option<&str>) -> bool {
        let has = |hay: &str, needle: &str| hay.to_ascii_lowercase().contains(&needle.to_ascii_lowercase());
        if let Some(s) = section
            && !has(&self.section, s)
        {
            return false;
        }
        match query {
            Some(q) => has(&self.path, q) || has(&self.title, q) || has(&self.description, q) || self.content.as_deref().is_some_and(|c| has(c, q)),
            None => true,
        }
    }

    /// Read metadata from a markdown file on disk.
    pub fn from_file(base_dir: &Path, file_path: &Path) -> Result<Self, String> {
        let rel = match file_path.strip_prefix(base_dir) {
            Ok(p) => p.to_string_lossy().replace('\\', "/"),
            Err(e) => return Err(format!("path is not inside base directory: {e}")),
        };
        let name = file_path.file_stem().and_then(|s| s.to_str()).unwrap_or(&rel).to_string();
        let section = derive_section(&rel);
        let uri = format!("docs://{rel}");

        let sample = read_sample(file_path, SAMPLE_BYTES).unwrap_or_default();
        let (title, description) = extract_metadata(&sample, &name);

        Ok(Self { uri, path: rel, name, title, description, section, content: None, source_path: Some(file_path.to_path_buf()) })
    }

    /// Read the full markdown text of the document.
    pub fn read_content(&self) -> Result<String, String> {
        if let Some(ref c) = self.content {
            return Ok(c.clone());
        }
        if let Some(ref p) = self.source_path {
            return read_limited_text(p, MAX_DOC_BYTES);
        }
        Err(format!("no content available for `{}`", self.path))
    }
}

/// Derive section from path components: `skills`, `styles` or `reference` anywhere in the path,
/// else the top folder, else `general`.
fn derive_section(rel_path: &str) -> String {
    let parts: Vec<&str> = rel_path.split('/').filter(|p| !p.is_empty()).collect();
    if let Some(known) = ["skills", "styles", "reference"].into_iter().find(|k| parts.contains(k)) {
        return known.to_string();
    }
    match parts.as_slice() {
        [dir, _, ..] => (*dir).to_string(),
        _ => "general".to_string(),
    }
}

/// Find one document by path or `docs://` URI. An exact path wins; otherwise a file name, a stem
/// or a trailing part of the path must match exactly one document, and several matches are an
/// error that lists them.
pub fn find<'a>(docs: &'a [Doc], target: &str) -> Result<&'a Doc, String> {
    let norm = target.trim().trim_start_matches("docs://").replace('\\', "/");
    let norm = norm.trim_start_matches("./").trim_start_matches('/');
    if let Some(d) = docs.iter().find(|d| d.path == norm || d.uri == target) {
        return Ok(d);
    }
    let suffix = format!("/{norm}");
    let hits: Vec<&Doc> = docs.iter().filter(|d| d.name == norm || d.path.ends_with(&suffix)).collect();
    match hits.as_slice() {
        [one] => Ok(one),
        [] => Err(format!("no document `{target}`")),
        many => {
            let paths: Vec<&str> = many.iter().map(|d| d.path.as_str()).collect();
            Err(format!("`{target}` matches several documents: {}; give the full path", paths.join(", ")))
        }
    }
}

/// Convert a kebab-case or snake_case identifier to a readable title.
fn humanize_name(name: &str) -> String {
    let mut words = Vec::new();
    for part in name.split(['-', '_']) {
        if part.is_empty() {
            continue;
        }
        let mut chars = part.chars();
        if let Some(first) = chars.next() {
            let mut word = first.to_uppercase().to_string();
            word.push_str(chars.as_str());
            words.push(word);
        }
    }
    if words.is_empty() { name.to_string() } else { words.join(" ") }
}

/// Extract title and description from markdown text.
fn extract_metadata(text: &str, default_name: &str) -> (String, String) {
    let lines: Vec<&str> = text.lines().map(str::trim).collect();
    let mut title = None;
    let mut description = None;

    // Check frontmatter
    let mut idx = 0;
    if lines.first().is_some_and(|l| *l == "---") {
        idx = 1;
        while idx < lines.len() && lines[idx] != "---" && lines[idx] != "..." {
            let line = lines[idx];
            if let Some((k, v)) = line.split_once(':') {
                let key = k.trim().to_ascii_lowercase();
                let val = v.trim().trim_matches(|c| c == '"' || c == '\'');
                if key == "title" && !val.is_empty() && title.is_none() {
                    title = Some(val.to_string());
                } else if key == "description" && !val.is_empty() && description.is_none() {
                    description = Some(val.to_string());
                }
            }
            idx = idx.saturating_add(1);
        }
        if idx < lines.len() {
            idx = idx.saturating_add(1);
        }
    }

    // Parse heading and first paragraph if not found in frontmatter
    while idx < lines.len() {
        let line = lines[idx];
        if title.is_none() {
            if let Some(rest) = line.strip_prefix("# ") {
                let t = rest.trim();
                if !t.is_empty() {
                    title = Some(t.to_string());
                    idx = idx.saturating_add(1);
                    continue;
                }
            } else if let Some(rest) = line.strip_prefix("## ") {
                let t = rest.trim();
                if !t.is_empty() {
                    title = Some(t.to_string());
                    idx = idx.saturating_add(1);
                    continue;
                }
            }
        }

        if description.is_none()
            && !line.is_empty()
            && !line.starts_with('#')
            && !line.starts_with("```")
            && !line.starts_with("---")
            && !line.starts_with("![")
            && !line.starts_with('|')
        {
            let desc = if line.chars().count() > 250 {
                let mut d = line.chars().take(247).collect::<String>();
                d.push_str("...");
                d
            } else {
                line.to_string()
            };
            description = Some(desc);
            break;
        }

        idx = idx.saturating_add(1);
    }

    let final_title = title.unwrap_or_else(|| humanize_name(default_name));
    let final_desc = description.unwrap_or_else(|| format!("Documentation for {final_title}"));
    (final_title, final_desc)
}

/// Read sample bytes from file for metadata extraction.
fn read_sample(path: &Path, max_bytes: usize) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let mut buf = Vec::new();
    let mut take = (&mut file).take(max_bytes as u64);
    take.read_to_end(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// Read text from file up to max_bytes.
pub fn read_limited_text(path: &Path, max_bytes: usize) -> Result<String, String> {
    let mut file = File::open(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    let mut buf = Vec::new();
    let mut take = (&mut file).take(max_bytes.saturating_add(1) as u64);
    take.read_to_end(&mut buf).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    if buf.len() > max_bytes {
        return Err(format!("document exceeds maximum allowed size of {max_bytes} bytes"));
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// Load all markdown documents under `dir` (symbolic links are skipped), sorted by path.
pub fn load_docs_from_dir(dir: &Path) -> Vec<Doc> {
    let mut docs = Vec::new();
    if dir.is_dir() {
        walk_dir(dir, dir, 0, &mut docs);
    }
    docs.sort_by(|a, b| a.path.cmp(&b.path));
    docs
}

fn walk_dir(base: &Path, current: &Path, depth: usize, out: &mut Vec<Doc>) {
    if depth > MAX_WALK_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(current) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let file_name = entry.file_name();
        let name_str = file_name.to_string_lossy();
        if name_str.starts_with('.') || name_str == "target" || name_str == "node_modules" {
            continue;
        }
        // never follow a link out of the directory
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_dir() {
            walk_dir(base, &path, depth.saturating_add(1), out);
        } else if kind.is_file() {
            let is_md =
                path.extension().and_then(|e| e.to_str()).is_some_and(|ext| ext.eq_ignore_ascii_case("md") || ext.eq_ignore_ascii_case("markdown"));
            if is_md && let Ok(doc) = Doc::from_file(base, &path) {
                out.push(doc);
            }
        }
    }
}
