//! Find in a document: case-insensitive (ASCII) matches, highlighted with a background.

use std::ops::Range;

use egui::Color32;
use egui::text::{ByteIndex, LayoutJob};

/// Longest query searched for (longer ones are cut, at a character boundary).
pub const MAX_QUERY: usize = 256;

/// The query as searched: cut to [`MAX_QUERY`] bytes.
pub(super) fn clamp(query: &str) -> &str {
    let mut end = query.len().min(MAX_QUERY);
    while !query.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    query.get(..end).unwrap_or_default()
}

/// Byte ranges of `query` in `text`, left to right and not overlapping, ignoring ASCII case.
/// An empty query matches nothing.
pub fn find_ranges<'a>(text: &'a str, query: &'a str) -> impl Iterator<Item = Range<usize>> + 'a {
    let q = clamp(query).as_bytes();
    let t = text.as_bytes();
    let first = q.first().map(u8::to_ascii_lowercase);
    let mut at = 0usize;
    std::iter::from_fn(move || {
        let first = first?;
        while let Some(rest) = t.get(at..) {
            let i = at;
            if rest.len() < q.len() {
                return None;
            }
            // the first byte of a match is never a UTF-8 continuation byte (only ASCII folds)
            if rest.first().map(u8::to_ascii_lowercase) == Some(first) && rest.get(..q.len()).is_some_and(|w| w.eq_ignore_ascii_case(q)) {
                at = i + q.len();
                return Some(i..i + q.len());
            }
            at = i + 1;
        }
        None
    })
}

/// Give the matches of `query` in `job` a background (splitting its sections where needed).
pub(super) fn mark(job: &mut LayoutJob, query: &str, bg: Color32) {
    let ranges: Vec<Range<usize>> = find_ranges(&job.text, query).collect();
    if ranges.is_empty() {
        return;
    }
    let mut out = Vec::with_capacity(job.sections.len() + ranges.len() * 2);
    let mut r = ranges.iter().peekable();
    for sec in job.sections.drain(..) {
        let mut at = sec.byte_range.start.0;
        let end = sec.byte_range.end.0;
        while at < end {
            // skip matches that end before here
            while r.peek().is_some_and(|m| m.end <= at) {
                r.next();
            }
            let (cut, hit) = match r.peek() {
                Some(m) if m.start <= at => (m.end.min(end), true),
                Some(m) => (m.start.min(end), false),
                None => (end, false),
            };
            let mut piece = sec.clone();
            piece.byte_range = ByteIndex(at)..ByteIndex(cut);
            if at == sec.byte_range.start.0 {
                piece.leading_space = sec.leading_space;
            } else {
                piece.leading_space = 0.0;
            }
            if hit {
                piece.format.background = bg;
            }
            out.push(piece);
            at = cut;
        }
    }
    job.sections = out;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_ignore_case_and_do_not_overlap() {
        let r: Vec<_> = find_ranges("Aaa aA é", "aa").collect();
        assert_eq!(r, vec![0..2, 4..6]);
        assert_eq!(find_ranges("abc", "").count(), 0);
        assert_eq!(find_ranges("é", "\u{e9}").collect::<Vec<_>>(), vec![0..2]);
        assert_eq!(find_ranges("ab", "abc").count(), 0);
    }

    #[test]
    fn marking_splits_sections() {
        let mut job = LayoutJob::default();
        job.append("hello ", 0.0, Default::default());
        job.append("world", 0.0, egui::TextFormat { color: Color32::BLUE, ..Default::default() });
        mark(&mut job, "lo w", Color32::RED);
        let parts: Vec<_> = job.sections.iter().map(|s| (s.byte_range.start.0..s.byte_range.end.0, s.format.background == Color32::RED)).collect();
        assert_eq!(parts, vec![(0..3, false), (3..6, true), (6..7, true), (7..11, false)]);
    }
}
