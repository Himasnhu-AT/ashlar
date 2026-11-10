//! Unified-patch parsing and writing (`git diff`, `diff -u`).
//!
//! Text before the first file (a commit message, an mbox header) and lines between files that no
//! rule claims are skipped. Inside a hunk, the header's line counts must match the body exactly;
//! anything else is a [`PatchError`], never a panic. Nothing is allocated from a number the
//! patch states: memory follows the input's length.

use std::fmt;

use crate::{FileDiff, FileStatus, Hunk, Line, LineKind, emphasize};

/// Why a patch did not parse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PatchErrorKind {
    /// An `@@` line that is not `@@ -a[,b] +c[,d] @@`.
    BadHunkHeader,
    /// A hunk before any file header.
    HunkWithoutFile,
    /// The patch ends before a hunk has all the lines its header announces.
    UnexpectedEnd,
    /// A hunk line that does not start with ` `, `-`, `+` or `\`, or more lines of one side than
    /// the header announces.
    BadHunkLine,
    /// A line number past `u32::MAX`.
    TooLarge,
}

/// A patch that did not parse, and where.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PatchError {
    /// 1-based line of the patch.
    pub line: usize,
    pub kind: PatchErrorKind,
}

impl fmt::Display for PatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let what = match self.kind {
            PatchErrorKind::BadHunkHeader => "malformed hunk header",
            PatchErrorKind::HunkWithoutFile => "hunk without a file header",
            PatchErrorKind::UnexpectedEnd => "patch ends inside a hunk",
            PatchErrorKind::BadHunkLine => "malformed hunk line",
            PatchErrorKind::TooLarge => "line number too large",
        };
        write!(f, "line {}: {what}", self.line)
    }
}

impl std::error::Error for PatchError {}

/// Extended header lines `git diff` writes between `diff --git` and `---`.
const GIT_HEADERS: &[&str] = &[
    "old mode ",
    "new mode ",
    "deleted file mode ",
    "new file mode ",
    "similarity index ",
    "dissimilarity index ",
    "rename from ",
    "rename to ",
    "copy from ",
    "copy to ",
    "index ",
    "Binary files ",
];

/// Parses a unified patch into one [`FileDiff`] per file, with word-level emphasis.
pub fn parse_patch(text: &str) -> Result<Vec<FileDiff>, PatchError> {
    let lines: Vec<&str> = text.split_inclusive('\n').map(|l| l.strip_suffix('\n').unwrap_or(l)).collect();
    let mut files: Vec<FileDiff> = Vec::new();
    let mut cur: Option<FileDiff> = None;
    let mut i = 0usize;
    while let Some(&line) = lines.get(i) {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            files.extend(cur.take());
            let mut f = FileDiff { header: vec![line.to_owned()], ..FileDiff::default() };
            if let Some((a, b)) = git_paths(rest) {
                f.old_path = Some(a);
                f.new_path = Some(b);
            }
            i += 1;
            i = git_header(&lines, i, &mut f);
            cur = Some(f);
        } else if line.starts_with("--- ") && lines.get(i + 1).is_some_and(|l| l.starts_with("+++ ")) {
            files.extend(cur.take());
            let mut f = FileDiff::default();
            i = file_lines(&lines, i, &mut f);
            if f.old_path.is_none() {
                f.status = FileStatus::Added;
            } else if f.new_path.is_none() {
                f.status = FileStatus::Deleted;
            }
            cur = Some(f);
        } else if line.starts_with("@@ ") {
            let Some(f) = cur.as_mut() else { return Err(PatchError { line: i + 1, kind: PatchErrorKind::HunkWithoutFile }) };
            let (hunk, next) = hunk(&lines, i)?;
            f.hunks.push(hunk);
            i = next;
        } else {
            i += 1;
        }
    }
    files.extend(cur);
    for f in &mut files {
        for h in &mut f.hunks {
            emphasize(h);
        }
    }
    Ok(files)
}

/// Reads the extended header after `diff --git`, up to and including `+++` (or the binary data).
/// Returns the index of the first line after it.
fn git_header(lines: &[&str], mut i: usize, f: &mut FileDiff) -> usize {
    while let Some(&line) = lines.get(i) {
        if line.starts_with("--- ") && lines.get(i + 1).is_some_and(|l| l.starts_with("+++ ")) {
            return file_lines(lines, i, f);
        }
        if line == "GIT binary patch" {
            f.binary = true;
            // the data runs to the next file
            while let Some(&l) = lines.get(i) {
                if l.starts_with("diff --git ") {
                    break;
                }
                f.header.push(l.to_owned());
                i += 1;
            }
            return i;
        }
        let Some(prefix) = GIT_HEADERS.iter().find(|p| line.starts_with(**p)) else { return i };
        let value = line.get(prefix.len()..).unwrap_or("");
        match *prefix {
            "old mode " => f.old_mode = Some(value.to_owned()),
            "new mode " => f.new_mode = Some(value.to_owned()),
            "deleted file mode " => {
                f.status = FileStatus::Deleted;
                f.old_mode = Some(value.to_owned());
                f.new_path = None;
            }
            "new file mode " => {
                f.status = FileStatus::Added;
                f.new_mode = Some(value.to_owned());
                f.old_path = None;
            }
            "similarity index " => f.similarity = value.strip_suffix('%').and_then(|v| v.parse().ok()),
            "rename from " => {
                f.status = FileStatus::Renamed;
                f.old_path = Some(unquote(value));
            }
            "rename to " => f.new_path = Some(unquote(value)),
            "copy from " => {
                f.status = FileStatus::Copied;
                f.old_path = Some(unquote(value));
            }
            "copy to " => f.new_path = Some(unquote(value)),
            "Binary files " => f.binary = true,
            _ => {}
        }
        f.header.push(line.to_owned());
        i += 1;
    }
    i
}

/// Reads a `---` / `+++` pair at `i` into the header and paths; returns the index after it.
fn file_lines(lines: &[&str], i: usize, f: &mut FileDiff) -> usize {
    let old = lines.get(i).copied().unwrap_or("");
    let new = lines.get(i + 1).copied().unwrap_or("");
    f.header.push(old.to_owned());
    f.header.push(new.to_owned());
    f.old_path = header_path(old.get(4..).unwrap_or(""), "a/");
    f.new_path = header_path(new.get(4..).unwrap_or(""), "b/");
    i + 2
}

/// The path of a `---`/`+++` line: no timestamp, unquoted, `/dev/null` is `None`, prefix stripped.
fn header_path(raw: &str, prefix: &str) -> Option<String> {
    let raw = if raw.starts_with('"') { raw } else { raw.split('\t').next().unwrap_or(raw) };
    let p = unquote(raw.trim_end_matches(['\r']));
    if p == "/dev/null" {
        return None;
    }
    Some(p.strip_prefix(prefix).map(str::to_owned).unwrap_or(p))
}

/// The paths of `diff --git a/x b/y`, prefixes stripped. With spaces in unquoted paths it is
/// ambiguous; the `---`/`+++` and rename lines that follow override it.
fn git_paths(rest: &str) -> Option<(String, String)> {
    if rest.starts_with('"') {
        let end = quoted_end(rest)?;
        let a = unquote(rest.get(..end)?);
        let b = unquote(rest.get(end..)?.trim_start());
        return Some((strip(&a, "a/"), strip(&b, "b/")));
    }
    // "a/P b/P": the same path twice splits in the middle
    let half = rest.len() / 2;
    if rest.len() % 2 == 1
        && let (Some(a), Some(b)) = (rest.get(..half), rest.get(half + 1..))
        && a.strip_prefix("a/").is_some_and(|x| Some(x) == b.strip_prefix("b/"))
    {
        return Some((strip(a, "a/"), strip(b, "b/")));
    }
    let at = rest.find(" b/")?;
    let a = rest.get(..at)?;
    let b = rest.get(at + 1..)?;
    Some((strip(a, "a/"), strip(&unquote(b), "b/")))
}

fn strip(p: &str, prefix: &str) -> String {
    p.strip_prefix(prefix).unwrap_or(p).to_owned()
}

/// Byte index just past the closing quote of a C-quoted string at the start of `s`.
fn quoted_end(s: &str) -> Option<usize> {
    let mut escaped = false;
    for (i, c) in s.char_indices().skip(1) {
        match c {
            _ if escaped => escaped = false,
            '\\' => escaped = true,
            '"' => return Some(i + 1),
            _ => {}
        }
    }
    None
}

/// Undoes git's C-style path quoting (`"a/sp\303\244ce \"x\""`); other strings pass through.
fn unquote(s: &str) -> String {
    let Some(inner) = s.strip_prefix('"').and_then(|x| x.strip_suffix('"')) else { return s.to_owned() };
    let mut bytes: Vec<u8> = Vec::with_capacity(inner.len());
    let mut it = inner.bytes().peekable();
    while let Some(c) = it.next() {
        if c != b'\\' {
            bytes.push(c);
            continue;
        }
        match it.next() {
            Some(b'n') => bytes.push(b'\n'),
            Some(b't') => bytes.push(b'\t'),
            Some(b'r') => bytes.push(b'\r'),
            Some(b'a') => bytes.push(7),
            Some(b'b') => bytes.push(8),
            Some(b'f') => bytes.push(12),
            Some(b'v') => bytes.push(11),
            Some(d @ b'0'..=b'7') => {
                let mut v = u32::from(d - b'0');
                for _ in 0..2 {
                    match it.peek() {
                        Some(&n @ b'0'..=b'7') => {
                            v = v * 8 + u32::from(n - b'0');
                            it.next();
                        }
                        _ => break,
                    }
                }
                bytes.push(u8::try_from(v).unwrap_or(b'?'));
            }
            Some(other) => bytes.push(other),
            None => bytes.push(b'\\'),
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// `a` or `a,b` (the count defaults to 1).
fn range(s: &str) -> Option<(u32, u32)> {
    let (start, len) = match s.split_once(',') {
        Some((a, b)) => (a, Some(b)),
        None => (s, None),
    };
    let digits = |x: &str| !x.is_empty() && x.bytes().all(|b| b.is_ascii_digit());
    if !digits(start) || len.is_some_and(|l| !digits(l)) {
        return None;
    }
    Some((start.parse().ok()?, len.map_or(Some(1), |l| l.parse().ok())?))
}

/// `@@ -a,b +c,d @@ section`.
fn hunk_header(line: &str) -> Option<Hunk> {
    let rest = line.strip_prefix("@@ -")?;
    let (old, rest) = rest.split_once(" +")?;
    let (new, rest) = rest.split_once(" @@")?;
    let (old_start, old_len) = range(old)?;
    let (new_start, new_len) = range(new)?;
    let section = rest.strip_prefix(' ').unwrap_or(rest).to_owned();
    Some(Hunk { old_start, old_len, new_start, new_len, section, lines: Vec::new() })
}

/// Reads the hunk whose header is at `i`; returns it and the index after it.
fn hunk(lines: &[&str], i: usize) -> Result<(Hunk, usize), PatchError> {
    let err = |at: usize, kind| PatchError { line: at + 1, kind };
    let mut h = hunk_header(lines.get(i).copied().unwrap_or("")).ok_or(err(i, PatchErrorKind::BadHunkHeader))?;
    let (mut old_left, mut new_left) = (h.old_len, h.new_len);
    let (mut old_no, mut new_no) = (h.old_range().start, h.new_range().start);
    let mut j = i + 1;
    while old_left > 0 || new_left > 0 {
        let Some(&line) = lines.get(j) else { return Err(err(j, PatchErrorKind::UnexpectedEnd)) };
        let (kind, text) = match line.as_bytes().first() {
            // an empty line is a context line whose trailing space an editor stripped
            None => (LineKind::Context, ""),
            Some(b' ') => (LineKind::Context, line.get(1..).unwrap_or("")),
            Some(b'-') => (LineKind::Removed, line.get(1..).unwrap_or("")),
            Some(b'+') => (LineKind::Added, line.get(1..).unwrap_or("")),
            Some(b'\\') => {
                if let Some(last) = h.lines.last_mut() {
                    last.no_newline = true;
                }
                j += 1;
                continue;
            }
            Some(_) => return Err(err(j, PatchErrorKind::BadHunkLine)),
        };
        let take_old = kind != LineKind::Added;
        let take_new = kind != LineKind::Removed;
        if (take_old && old_left == 0) || (take_new && new_left == 0) {
            return Err(err(j, PatchErrorKind::BadHunkLine));
        }
        let (mut o, mut n) = (None, None);
        if take_old {
            o = Some(old_no);
            old_no = old_no.checked_add(1).ok_or(err(j, PatchErrorKind::TooLarge))?;
            old_left -= 1;
        }
        if take_new {
            n = Some(new_no);
            new_no = new_no.checked_add(1).ok_or(err(j, PatchErrorKind::TooLarge))?;
            new_left -= 1;
        }
        h.lines.push(Line { kind, old_no: o, new_no: n, text: text.to_owned(), no_newline: false, emphasis: Vec::new() });
        j += 1;
    }
    // a marker after the hunk's last line
    if lines.get(j).is_some_and(|l| l.starts_with('\\')) {
        if let Some(last) = h.lines.last_mut() {
            last.no_newline = true;
        }
        j += 1;
    }
    Ok((h, j))
}

/// Writes `file` as a unified patch into `out`.
pub(crate) fn write_file(file: &FileDiff, out: &mut String) {
    for l in &file.header {
        out.push_str(l);
        out.push('\n');
    }
    for h in &file.hunks {
        out.push_str(&h.header());
        out.push('\n');
        for l in &h.lines {
            out.push(match l.kind {
                LineKind::Context => ' ',
                LineKind::Removed => '-',
                LineKind::Added => '+',
            });
            out.push_str(&l.text);
            out.push('\n');
            if l.no_newline {
                out.push_str("\\ No newline at end of file\n");
            }
        }
    }
}

/// Writes files as one unified patch (the inverse of [`parse_patch`] for `git diff` output).
pub fn write_patch(files: &[FileDiff]) -> String {
    let mut out = String::new();
    for f in files {
        write_file(f, &mut out);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hunk_headers() {
        let h = hunk_header("@@ -1,3 +4 @@ fn main() {").unwrap();
        assert_eq!((h.old_start, h.old_len, h.new_start, h.new_len), (1, 3, 4, 1));
        assert_eq!(h.section, "fn main() {");
        assert!(hunk_header("@@ -1,x +4 @@").is_none());
        assert!(hunk_header("@@ -99999999999 +1 @@").is_none());
        assert!(hunk_header("@@ -+1 +1 @@").is_none());
    }

    #[test]
    fn paths() {
        assert_eq!(git_paths("a/x y b/x y"), Some(("x y".into(), "x y".into())));
        assert_eq!(git_paths("a/old b/new"), Some(("old".into(), "new".into())));
        assert_eq!(git_paths("\"a/t\\303\\244b\" \"b/t\\303\\244b\""), Some(("täb".into(), "täb".into())));
        assert_eq!(header_path("a/f.rs\t2024-01-01", "a/"), Some("f.rs".into()));
        assert_eq!(header_path("/dev/null", "a/"), None);
        assert_eq!(unquote("\"\\\"q\\\\\\n\""), "\"q\\\n");
    }

    #[test]
    fn malformed() {
        let e = |p: &str| parse_patch(p).unwrap_err().kind;
        assert_eq!(e("@@ -1 +1 @@\n-a\n+b\n"), PatchErrorKind::HunkWithoutFile);
        assert_eq!(e("--- a\n+++ b\n@@ -1,2 +1 @@\n-a\n"), PatchErrorKind::UnexpectedEnd);
        assert_eq!(e("--- a\n+++ b\n@@ -1 +1 @@\n*a\n"), PatchErrorKind::BadHunkLine);
        assert_eq!(e("--- a\n+++ b\n@@ -1 +1 @@\n-a\n-b\n"), PatchErrorKind::BadHunkLine);
        assert_eq!(e("--- a\n+++ b\n@@ -1 +1\n"), PatchErrorKind::BadHunkHeader);
        assert_eq!(e("--- a\n+++ b\n@@ -4294967295,2 +1,2 @@\n a\n b\n"), PatchErrorKind::TooLarge);
    }
}
