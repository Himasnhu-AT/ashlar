//! Unified diffs and patches: headers, hunk markers, added and removed lines.

use crate::scan::Cur;
use crate::scan::Out;
use crate::{Kind, State};

const HEADERS: &[&str] =
    &["diff ", "index ", "--- ", "+++ ", "new file", "deleted file", "similarity", "rename ", "old mode", "new mode", "Binary files"];

pub(crate) fn lex(line: &str, _st: State, out: &mut Out<'_>) -> State {
    if HEADERS.iter().any(|h| line.starts_with(h)) {
        out.tok(0, line.len(), Kind::Meta);
        return State::START;
    }
    let c = Cur::new(line);
    let kind = match line.as_bytes().first() {
        Some(b'@') if line.starts_with("@@") => {
            // `@@ -1,2 +1,3 @@ context`: the marker is meta, the context after it plain
            let end = c.find(b" @@").map_or(line.len(), |e| e + 3);
            out.tok(0, end, Kind::Meta);
            return State::START;
        }
        Some(b'+') => Kind::Inserted,
        Some(b'-') => Kind::Deleted,
        Some(b'\\') => Kind::Comment,
        _ => Kind::Plain,
    };
    out.tok(0, line.len(), kind);
    State::START
}
