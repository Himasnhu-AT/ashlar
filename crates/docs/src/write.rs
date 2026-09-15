//! Writing pages into the user's own folder (the one [`Library::overlay_dir`] reads), so an agent
//! can keep notes that later sessions find: save a page, append to one, delete one.
//!
//! Only markdown files strictly inside that folder are touched: a path must be relative, made of
//! plain names (letters, digits, `.`, `_`, `-`, no leading dot), end in `.md` and stay within
//! [`MAX_DEPTH`] folders; no part of it may be a symbolic link. A page needs `title` and
//! `description` frontmatter (so it lists like any other), is at most [`MAX_WRITE_BYTES`], and is
//! written atomically (a temporary file in the same folder, then a rename). Each write sets an
//! `updated: YYYY-MM-DD` frontmatter line.
//!
//! A path that names a built-in page (or a built-in category's `_category.md`) is refused unless
//! the caller asks to replace it: the user's page then hides the built-in one, as any overlay
//! page does. Built-in pages cannot be deleted; deleting the user's copy brings the built-in back.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use crate::{CATEGORY_FILE, Frontmatter, Library, MAX_DEPTH, Source};

/// Largest page [`save`] and [`append`] write.
pub const MAX_WRITE_BYTES: usize = 256 * 1024;
/// Longest accepted path.
const MAX_PATH: usize = 240;

/// What a write did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Written {
    /// The page's path in the library, e.g. `memory/style.md`.
    pub path: String,
    /// The file on disk.
    pub file: PathBuf,
    /// Whether the file is new.
    pub created: bool,
    /// Bytes written.
    pub bytes: usize,
    /// Whether it now hides a built-in page of the same path.
    pub replaces_built_in: bool,
}

/// Check a page path for writing; the normalized path and its parts.
pub fn check_path(path: &str) -> Result<(String, Vec<String>), String> {
    let p = path.trim().trim_start_matches("docs://");
    if p.is_empty() {
        return Err("give a path such as `memory/style.md`".into());
    }
    if p.len() > MAX_PATH {
        return Err(format!("the path is longer than {MAX_PATH} characters"));
    }
    if p.starts_with('/') || p.starts_with('\\') || p.starts_with('~') || p.contains(':') || p.contains('\\') {
        return Err(format!("`{p}` must be a path relative to your documentation folder, with `/` between folders"));
    }
    let parts: Vec<String> = p.split('/').map(str::to_string).collect();
    if parts.len() > MAX_DEPTH {
        return Err(format!("`{p}` is nested deeper than {MAX_DEPTH} folders"));
    }
    for part in &parts {
        let plain = !part.is_empty() && !part.starts_with('.') && part.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
        if !plain {
            return Err(format!(
                "`{p}`: `{part}` is not allowed (use letters, digits, `.`, `_` and `-`; no empty parts, `..` or names starting with a dot)"
            ));
        }
    }
    if !p.ends_with(".md") {
        return Err(format!("`{p}` must be a markdown file ending in `.md`"));
    }
    Ok((parts.join("/"), parts))
}

/// Whether `path` is a page (or a described category's `_category.md`) built into `lib`.
fn built_in(lib: &Library, path: &str) -> bool {
    let (dir, name) = path.rsplit_once('/').unwrap_or(("", path));
    if name == CATEGORY_FILE {
        return lib.described().iter().any(|c| c.path == dir && c.source == Source::BuiltIn);
    }
    lib.docs().iter().any(|d| d.path == path && d.source == Source::BuiltIn)
}

/// The file for `parts` under `dir`, creating its folders; refuses symbolic links on the way.
fn file_for(dir: &Path, parts: &[String]) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let mut at = dir.to_path_buf();
    let (name, folders) = parts.split_last().ok_or("empty path")?;
    for f in folders {
        at.push(f);
        match std::fs::symlink_metadata(&at) {
            Ok(m) if m.file_type().is_symlink() => return Err(format!("`{f}` is a symbolic link; pages are only written inside the folder")),
            Ok(m) if !m.is_dir() => return Err(format!("`{f}` is a file, not a folder")),
            Ok(_) => {}
            Err(_) => std::fs::create_dir(&at).map_err(|e| format!("cannot create {}: {e}", at.display()))?,
        }
    }
    at.push(name);
    if let Ok(m) = std::fs::symlink_metadata(&at)
        && (m.file_type().is_symlink() || !m.is_file())
    {
        return Err(format!("`{name}` is not a regular file; it is left alone"));
    }
    Ok(at)
}

/// Check frontmatter and size, and set `updated`.
fn prepare(path: &str, content: &str) -> Result<String, String> {
    let front = Frontmatter::parse(content);
    if front.title.is_none() || front.description.is_none() {
        return Err(format!(
            "`{path}` needs frontmatter with `title` and `description` (when to read it), e.g. \"---\\ntitle: Editing style\\ndescription: Read before cutting\\n---\\n…\""
        ));
    }
    let text = with_updated(content, &today());
    if text.len() > MAX_WRITE_BYTES {
        return Err(format!("`{path}` is {} bytes; a page holds at most {MAX_WRITE_BYTES} (keep it short: replace old notes)", text.len()));
    }
    Ok(text)
}

/// Write `text` to `file` through a temporary file in the same folder.
fn write_atomic(file: &Path, text: &str) -> Result<(), String> {
    let parent = file.parent().ok_or("no folder")?;
    let name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = parent.join(format!(".{name}.{}.tmp", std::process::id()));
    let result = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        std::fs::rename(&tmp, file)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result.map_err(|e| format!("cannot write {}: {e}", file.display()))
}

/// Save `content` as the page `path` in the user's folder `dir` (replacing the user's own page
/// of that path). `lib` is the library the folder overlays: a built-in path is refused unless
/// `replace_built_in`.
pub fn save(dir: &Path, lib: &Library, path: &str, content: &str, replace_built_in: bool) -> Result<Written, String> {
    let (path, parts) = check_path(path)?;
    let shadows = built_in(lib, &path);
    if shadows && !replace_built_in {
        return Err(format!(
            "`{path}` is a built-in page; save under another path (your notes go in `memory/`), or ask to replace it, which hides the built-in page until yours is deleted"
        ));
    }
    let text = prepare(&path, content)?;
    let file = file_for(dir, &parts)?;
    let created = !file.exists();
    write_atomic(&file, &text)?;
    Ok(Written { path, file, created, bytes: text.len(), replaces_built_in: shadows })
}

/// Add `text` to the end of the user's page `path` (it must exist: [`save`] makes it).
pub fn append(dir: &Path, path: &str, text: &str) -> Result<Written, String> {
    let (path, parts) = check_path(path)?;
    let file = file_for(dir, &parts)?;
    let old =
        std::fs::read_to_string(&file).map_err(|_| format!("`{path}` is not one of your pages yet; save it first (with title and description)"))?;
    let addition = text.trim_matches('\n');
    if addition.trim().is_empty() {
        return Err("nothing to append".into());
    }
    let joined = format!("{}\n\n{addition}\n", old.trim_end());
    let text = prepare(&path, &joined)?;
    write_atomic(&file, &text)?;
    Ok(Written { path, file, created: false, bytes: text.len(), replaces_built_in: false })
}

/// Delete the user's page `path`. A built-in page of the same path shows again.
pub fn delete(dir: &Path, lib: &Library, path: &str) -> Result<Written, String> {
    let (path, parts) = check_path(path)?;
    let file = file_for(dir, &parts)?;
    if !file.is_file() {
        let why = if built_in(lib, &path) { "it is a built-in page, which cannot be deleted" } else { "there is no such page of yours" };
        return Err(format!("`{path}` was not deleted: {why}"));
    }
    std::fs::remove_file(&file).map_err(|e| format!("cannot delete {}: {e}", file.display()))?;
    Ok(Written { replaces_built_in: built_in(lib, &path), path, file, created: false, bytes: 0 })
}

/// `content` with its frontmatter's `updated` line set to `date` (added when missing).
fn with_updated(content: &str, date: &str) -> String {
    let body_start = content.strip_prefix('\u{feff}').unwrap_or(content);
    let Some(rest) = body_start.strip_prefix("---\n") else { return content.to_string() };
    let mut out = String::from("---\n");
    let mut done = false;
    let mut lines = rest.split_inclusive('\n');
    for line in lines.by_ref() {
        let trimmed = line.trim_end();
        if trimmed == "---" || trimmed == "..." {
            if !done {
                out.push_str(&format!("updated: {date}\n"));
            }
            out.push_str(line);
            break;
        }
        if line.split_once(':').is_some_and(|(k, _)| k.trim() == "updated") {
            if !done {
                out.push_str(&format!("updated: {date}\n"));
                done = true;
            }
            continue;
        }
        out.push_str(line);
    }
    for line in lines {
        out.push_str(line);
    }
    out
}

/// Today's date (UTC) as `YYYY-MM-DD`.
fn today() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let (y, m, d) = civil(days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Days since 1970-01-01 → (year, month, day), proleptic Gregorian.
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days.saturating_add(719_468);
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, u32::try_from(m).unwrap_or(1), u32::try_from(d).unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(20_384), (2025, 10, 23));
        assert_eq!(civil(11_016), (2000, 2, 29));
        assert_eq!(today().len(), 10);
    }

    #[test]
    fn updated_is_set_once() {
        let a = with_updated("---\ntitle: T\ndescription: D\n---\nbody\n", "2026-09-14");
        assert_eq!(a, "---\ntitle: T\ndescription: D\nupdated: 2026-09-14\n---\nbody\n");
        let b = with_updated("---\ntitle: T\nupdated: 2020-01-01\ndescription: D\nupdated: x\n---\nbody\n", "2026-09-14");
        assert_eq!(b, "---\ntitle: T\nupdated: 2026-09-14\ndescription: D\n---\nbody\n");
    }
}
