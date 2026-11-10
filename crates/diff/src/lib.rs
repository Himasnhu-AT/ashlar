//! Headless text diffs for ashlar: line diffs with a cost cap, word-level changes, hunks with
//! context and collapsible unchanged ranges, and a unified-patch parser and writer.
//!
//! No dependencies, no UI; builds for `wasm32-unknown-unknown`. The view lives in
//! `ashlar_ui::diff_view`.
//!
//! ```
//! use ashlar_diff::{FileDiff, LineKind};
//!
//! let d = FileDiff::from_texts("a\nb\nc\n", "a\nB\nc\n").with_paths("x.txt", "x.txt");
//! let hunk = &d.hunks[0];
//! assert_eq!((hunk.old_start, hunk.old_len, hunk.new_start, hunk.new_len), (1, 3, 1, 3));
//! assert_eq!(hunk.lines[1].kind, LineKind::Removed);
//! assert_eq!(d.to_patch(), "--- a/x.txt\n+++ b/x.txt\n@@ -1,3 +1,3 @@\n a\n-b\n+B\n c\n");
//! ```
//!
//! Patches from `git diff` parse into one [`FileDiff`] per file and write back byte for byte:
//!
//! ```
//! let patch = "diff --git a/f b/f\nindex 1..2 100644\n--- a/f\n+++ b/f\n@@ -1 +1 @@\n-x\n+y\n";
//! let files = ashlar_diff::parse_patch(patch).unwrap();
//! assert_eq!(files[0].new_path.as_deref(), Some("f"));
//! assert_eq!(ashlar_diff::write_patch(&files), patch);
//! ```

mod myers;
mod patch;
mod words;

use std::ops::Range;

pub use myers::{DEFAULT_MAX_COST, Edit, Op, diff_ops};
pub use patch::{PatchError, PatchErrorKind, parse_patch, write_patch};
pub use words::{MAX_WORD_DIFF_LINE, word_diff};

/// What a diff line is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LineKind {
    /// Unchanged (shown for context).
    Context,
    /// Only in the old text.
    Removed,
    /// Only in the new text.
    Added,
}

/// One line of a hunk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub kind: LineKind,
    /// 1-based line number in the old text (`None` for added lines).
    pub old_no: Option<u32>,
    /// 1-based line number in the new text (`None` for removed lines).
    pub new_no: Option<u32>,
    /// The text, without its line break.
    pub text: String,
    /// The line has no line break (`\ No newline at end of file`).
    pub no_newline: bool,
    /// Byte ranges of `text` that changed against the paired line (word-level), sorted.
    pub emphasis: Vec<Range<usize>>,
}

/// A run of changes with the context around them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hunk {
    /// As in the `@@ -old_start,old_len +new_start,new_len @@` header: when a length is 0 the
    /// start is the line *before* the hunk.
    pub old_start: u32,
    pub old_len: u32,
    pub new_start: u32,
    pub new_len: u32,
    /// The text after the second `@@` (often the enclosing function), without the leading space.
    pub section: String,
    pub lines: Vec<Line>,
}

impl Hunk {
    /// The first new line number the hunk covers, and one past its last.
    pub fn new_range(&self) -> Range<u32> {
        let first = if self.new_len == 0 { self.new_start.saturating_add(1) } else { self.new_start };
        first..first.saturating_add(self.new_len)
    }

    /// The first old line number the hunk covers, and one past its last.
    pub fn old_range(&self) -> Range<u32> {
        let first = if self.old_len == 0 { self.old_start.saturating_add(1) } else { self.old_start };
        first..first.saturating_add(self.old_len)
    }

    /// The `@@ … @@` header line.
    pub fn header(&self) -> String {
        let range = |start: u32, len: u32| if len == 1 { start.to_string() } else { format!("{start},{len}") };
        let mut s = format!("@@ -{} +{} @@", range(self.old_start, self.old_len), range(self.new_start, self.new_len));
        if !self.section.is_empty() {
            s.push(' ');
            s.push_str(&self.section);
        }
        s
    }
}

/// How a file changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum FileStatus {
    #[default]
    Modified,
    Added,
    Deleted,
    Renamed,
    Copied,
}

/// An unchanged range between hunks (or before the first, or after the last), which a view
/// shows collapsed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gap {
    /// The hunk this gap comes before (`hunks.len()` for the gap after the last hunk).
    pub before_hunk: usize,
    /// First hidden line, 1-based, in the old and the new text.
    pub old_start: u32,
    pub new_start: u32,
    /// Number of hidden lines.
    pub len: u32,
    /// The lines' text is known ([`FileDiff::source_line`]), so the gap can be expanded.
    pub expandable: bool,
}

/// The new text of a file, indexed by line, so collapsed ranges can be expanded.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
struct Source {
    text: String,
    /// Byte offset where each line starts.
    starts: Vec<usize>,
}

impl Source {
    fn new(text: String) -> Source {
        let mut starts = vec![0];
        starts.extend(text.match_indices('\n').map(|(i, _)| i + 1).filter(|&i| i < text.len()));
        if text.is_empty() {
            starts.clear();
        }
        Source { text, starts }
    }

    fn line(&self, i: usize) -> Option<&str> {
        let start = *self.starts.get(i)?;
        let end = self.starts.get(i + 1).copied().unwrap_or(self.text.len());
        let s = self.text.get(start..end)?;
        Some(s.strip_suffix('\n').unwrap_or(s))
    }
}

/// Options for [`FileDiff::from_texts_with`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiffOptions {
    /// Unchanged lines kept around each change (default 3).
    pub context: u32,
    /// Cost cap of one Myers run (default [`DEFAULT_MAX_COST`]); see [`diff_ops`].
    pub max_cost: usize,
    /// Compute word-level emphasis for paired lines (default on).
    pub words: bool,
}

impl Default for DiffOptions {
    fn default() -> Self {
        DiffOptions { context: 3, max_cost: DEFAULT_MAX_COST, words: true }
    }
}

/// The changes to one file.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct FileDiff {
    /// Path before the change (`None` for an added file); `a/` and `b/` prefixes are stripped.
    pub old_path: Option<String>,
    /// Path after the change (`None` for a deleted file).
    pub new_path: Option<String>,
    pub status: FileStatus,
    /// A binary file: no hunks, only a marker.
    pub binary: bool,
    /// `similarity index` of a rename or copy, in percent.
    pub similarity: Option<u8>,
    pub old_mode: Option<String>,
    pub new_mode: Option<String>,
    pub hunks: Vec<Hunk>,
    /// The header lines as written (`diff --git`, `index`, `---`, `+++`, binary data…), without
    /// line breaks; [`FileDiff::to_patch`] writes them back verbatim.
    pub header: Vec<String>,
    /// The diff hit its cost cap: correct, but not necessarily the smallest.
    pub coarse: bool,
    source: Option<Source>,
}

impl FileDiff {
    /// Diffs two texts line by line with the default options.
    pub fn from_texts(old: &str, new: &str) -> FileDiff {
        FileDiff::from_texts_with(old, new, DiffOptions::default())
    }

    /// Diffs two texts line by line. The new text is kept, so every [`Gap`] is expandable.
    pub fn from_texts_with(old: &str, new: &str, opts: DiffOptions) -> FileDiff {
        let a: Vec<&str> = old.split_inclusive('\n').collect();
        let b: Vec<&str> = new.split_inclusive('\n').collect();
        let edit = diff_ops(&a, &b, opts.max_cost);
        let mut hunks = build_hunks(&a, &b, &edit, opts.context);
        if opts.words {
            for h in &mut hunks {
                emphasize(h);
            }
        }
        FileDiff {
            hunks,
            header: vec!["--- a".to_owned(), "+++ b".to_owned()],
            coarse: edit.coarse,
            source: Some(Source::new(new.to_owned())),
            ..FileDiff::default()
        }
    }

    /// Sets both paths (and the `---`/`+++` header lines).
    pub fn with_paths(mut self, old: &str, new: &str) -> FileDiff {
        self.old_path = Some(old.to_owned());
        self.new_path = Some(new.to_owned());
        self.header = vec![format!("--- a/{old}"), format!("+++ b/{new}")];
        if old != new && self.status == FileStatus::Modified {
            self.status = FileStatus::Renamed;
        }
        self
    }

    /// Supplies the full new text of a parsed patch, so its collapsed ranges can be expanded.
    pub fn with_source(mut self, new_text: &str) -> FileDiff {
        self.source = Some(Source::new(new_text.to_owned()));
        self
    }

    /// The path to show: the new one, else the old one.
    pub fn path(&self) -> &str {
        self.new_path.as_deref().or(self.old_path.as_deref()).unwrap_or("")
    }

    /// Lines added and removed.
    pub fn stats(&self) -> (usize, usize) {
        let mut added = 0usize;
        let mut removed = 0usize;
        for l in self.hunks.iter().flat_map(|h| &h.lines) {
            match l.kind {
                LineKind::Added => added += 1,
                LineKind::Removed => removed += 1,
                LineKind::Context => {}
            }
        }
        (added, removed)
    }

    /// Number of lines of the new text, when it is known.
    pub fn source_len(&self) -> Option<u32> {
        self.source.as_ref().map(|s| u32::try_from(s.starts.len()).unwrap_or(u32::MAX))
    }

    /// Line `new_no` (1-based) of the new text, without its line break, when the text is known.
    pub fn source_line(&self, new_no: u32) -> Option<&str> {
        let i = usize::try_from(new_no.checked_sub(1)?).ok()?;
        self.source.as_ref()?.line(i)
    }

    /// The unchanged ranges around the hunks, in order. The range after the last hunk is listed
    /// only when the new text is known (a patch does not say how long the file is). Empty ranges
    /// are left out.
    pub fn gaps(&self) -> Vec<Gap> {
        let expandable = self.source.is_some();
        let mut out = Vec::new();
        let (mut old_next, mut new_next) = (1u32, 1u32);
        for (i, h) in self.hunks.iter().enumerate() {
            let (o, n) = (h.old_range(), h.new_range());
            let len = n.start.saturating_sub(new_next);
            if len > 0 {
                out.push(Gap { before_hunk: i, old_start: old_next, new_start: new_next, len, expandable });
            }
            old_next = o.end;
            new_next = n.end;
        }
        if let Some(total) = self.source_len() {
            let len = total.saturating_add(1).saturating_sub(new_next);
            if len > 0 && !self.binary {
                out.push(Gap { before_hunk: self.hunks.len(), old_start: old_next, new_start: new_next, len, expandable });
            }
        }
        out
    }

    /// This file as a unified patch: the header lines, then the hunks.
    pub fn to_patch(&self) -> String {
        let mut out = String::new();
        patch::write_file(self, &mut out);
        out
    }
}

/// Builds hunks from a line edit script; `a` and `b` keep their line breaks.
fn build_hunks(a: &[&str], b: &[&str], edit: &Edit, context: u32) -> Vec<Hunk> {
    // one entry per line of the script: (kind, old index, new index)
    let mut rows: Vec<(LineKind, usize, usize)> = Vec::with_capacity(a.len().saturating_add(b.len()));
    for op in &edit.ops {
        match *op {
            Op::Equal { old, new, len } => rows.extend((0..len).map(|i| (LineKind::Context, old + i, new + i))),
            Op::Delete { old, len } => rows.extend((0..len).map(|i| (LineKind::Removed, old + i, 0))),
            Op::Insert { new, len } => rows.extend((0..len).map(|i| (LineKind::Added, 0, new + i))),
        }
    }
    let ctx = context as usize;
    let is_change = |i: usize| rows.get(i).is_some_and(|r| r.0 != LineKind::Context);
    let mut hunks = Vec::new();
    let mut i = 0usize;
    // old and new lines before `rows[i]`
    let (mut old_before, mut new_before) = (0usize, 0usize);
    let mut hunk_end = 0usize;
    while i < rows.len() {
        if !is_change(i) {
            i += 1;
            continue;
        }
        // extend over changes separated by at most 2·context unchanged lines
        let first = i;
        let mut last_end = i;
        let mut j = i;
        while j < rows.len() {
            if is_change(j) {
                last_end = j + 1;
                j += 1;
            } else if j - last_end < 2 * ctx && j + 1 < rows.len() {
                j += 1;
            } else {
                break;
            }
        }
        let lo = first.saturating_sub(ctx).max(hunk_end);
        let hi = last_end.saturating_add(ctx).min(rows.len());
        // count lines between the previous hunk's end and `lo`
        for r in rows.get(hunk_end..lo).unwrap_or(&[]) {
            count(r.0, &mut old_before, &mut new_before);
        }
        let mut lines = Vec::with_capacity(hi - lo);
        let (mut o, mut n) = (old_before, new_before);
        for &(kind, oi, ni) in rows.get(lo..hi).unwrap_or(&[]) {
            let raw = match kind {
                LineKind::Added => b.get(ni),
                _ => a.get(oi),
            }
            .copied()
            .unwrap_or("");
            let no_newline = !raw.ends_with('\n');
            let text = raw.strip_suffix('\n').unwrap_or(raw).to_owned();
            let num = |x: usize| u32::try_from(x + 1).ok();
            let (old_no, new_no) = match kind {
                LineKind::Context => (num(o), num(n)),
                LineKind::Removed => (num(o), None),
                LineKind::Added => (None, num(n)),
            };
            count(kind, &mut o, &mut n);
            lines.push(Line { kind, old_no, new_no, text, no_newline, emphasis: Vec::new() });
        }
        let old_len = o - old_before;
        let new_len = n - new_before;
        let start = |before: usize, len: usize| u32::try_from(if len == 0 { before } else { before + 1 }).unwrap_or(u32::MAX);
        hunks.push(Hunk {
            old_start: start(old_before, old_len),
            old_len: u32::try_from(old_len).unwrap_or(u32::MAX),
            new_start: start(new_before, new_len),
            new_len: u32::try_from(new_len).unwrap_or(u32::MAX),
            section: String::new(),
            lines,
        });
        old_before = o;
        new_before = n;
        hunk_end = hi;
        i = hi;
    }
    hunks
}

fn count(kind: LineKind, old: &mut usize, new: &mut usize) {
    match kind {
        LineKind::Context => {
            *old += 1;
            *new += 1;
        }
        LineKind::Removed => *old += 1,
        LineKind::Added => *new += 1,
    }
}

/// Word-level emphasis for a hunk: in each run of removed lines followed by added lines, the
/// n-th removed line pairs with the n-th added line.
pub(crate) fn emphasize(h: &mut Hunk) {
    let mut i = 0usize;
    let len = h.lines.len();
    while i < len {
        let removed = h.lines.iter().skip(i).take_while(|l| l.kind == LineKind::Removed).count();
        if removed == 0 {
            i += 1;
            continue;
        }
        let added = h.lines.iter().skip(i + removed).take_while(|l| l.kind == LineKind::Added).count();
        for k in 0..removed.min(added) {
            let (ri, ai) = (i + k, i + removed + k);
            let pair = match (h.lines.get(ri), h.lines.get(ai)) {
                (Some(r), Some(a)) => word_diff(&r.text, &a.text),
                _ => None,
            };
            if let Some((er, ea)) = pair {
                if let Some(r) = h.lines.get_mut(ri) {
                    r.emphasis = er;
                }
                if let Some(a) = h.lines.get_mut(ai) {
                    a.emphasis = ea;
                }
            }
        }
        i += removed + added;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hunks_merge_close_changes_and_split_far_ones() {
        let old: String = (1..=30).map(|i| format!("{i}\n")).collect();
        let new: String = (1..=30)
            .map(|i| match i {
                5 => "five\n".to_owned(),
                8 => "eight\n".to_owned(),
                25 => "x\n".to_owned(),
                _ => format!("{i}\n"),
            })
            .collect();
        let d = FileDiff::from_texts(&old, &new);
        assert_eq!(d.hunks.len(), 2);
        let h = &d.hunks[0];
        assert_eq!((h.old_start, h.old_len, h.new_start, h.new_len), (2, 10, 2, 10));
        let h = &d.hunks[1];
        assert_eq!((h.old_start, h.old_len), (22, 7));
        let gaps = d.gaps();
        assert_eq!(gaps.len(), 3);
        assert_eq!((gaps[0].new_start, gaps[0].len), (1, 1));
        assert_eq!((gaps[1].new_start, gaps[1].len), (12, 10));
        assert_eq!((gaps[2].new_start, gaps[2].len, gaps[2].before_hunk), (29, 2, 2));
        assert_eq!(d.source_line(12), Some("12"));
    }

    #[test]
    fn identical_texts_are_one_gap() {
        let d = FileDiff::from_texts("a\nb\n", "a\nb\n");
        assert!(d.hunks.is_empty());
        assert_eq!(d.gaps(), vec![Gap { before_hunk: 0, old_start: 1, new_start: 1, len: 2, expandable: true }]);
        assert!(FileDiff::from_texts("", "").gaps().is_empty());
    }

    #[test]
    fn missing_final_newline() {
        let d = FileDiff::from_texts("a\nb", "a\nb\n");
        let p = d.to_patch();
        assert_eq!(p, "--- a\n+++ b\n@@ -1,2 +1,2 @@\n a\n-b\n\\ No newline at end of file\n+b\n");
        let back = parse_patch(&p).unwrap();
        assert_eq!(back[0].hunks, d.hunks);
    }

    #[test]
    fn added_and_deleted_whole_files() {
        let d = FileDiff::from_texts("", "x\ny\n");
        assert_eq!(d.hunks[0].header(), "@@ -0,0 +1,2 @@");
        let d = FileDiff::from_texts("x\n", "");
        assert_eq!(d.hunks[0].header(), "@@ -1 +0,0 @@");
    }
}
