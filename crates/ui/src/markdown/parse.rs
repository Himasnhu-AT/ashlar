//! Block and inline parsing: line based, forgiving, and resumable for streaming text.

use super::{MAX_BYTES, Marker, MdBlock, Span};

fn fence(line: &str) -> Option<(&str, &str)> {
    let t = line.trim_start();
    for f in ["```", "~~~"] {
        if let Some(rest) = t.strip_prefix(f) {
            return Some((f, rest.trim_start_matches(f.chars().next().unwrap_or('`')).trim()));
        }
    }
    None
}

fn heading(line: &str) -> Option<(u8, &str)> {
    let t = line.trim_start();
    let hashes = t.chars().take_while(|c| *c == '#').count();
    if !(1..=6).contains(&hashes) {
        return None;
    }
    let rest = t.get(hashes..)?;
    if rest.is_empty() {
        return Some((hashes as u8, ""));
    }
    rest.starts_with(' ').then(|| (hashes as u8, rest.trim().trim_end_matches('#').trim_end()))
}

fn is_rule(line: &str) -> bool {
    let t: String = line.chars().filter(|c| !c.is_whitespace()).collect();
    t.len() >= 3 && (t.chars().all(|c| c == '-') || t.chars().all(|c| c == '*') || t.chars().all(|c| c == '_'))
}

/// `(depth, marker, text)` of a list item line.
fn item(line: &str) -> Option<(u8, Marker, &str)> {
    let indent = line.chars().take_while(|c| *c == ' ' || *c == '\t').map(|c| if c == '\t' { 4 } else { 1 }).sum::<usize>();
    let t = line.trim_start();
    let depth = (indent / 2).min(8) as u8;
    let (marker, rest) = if let Some(r) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")).or_else(|| t.strip_prefix("+ ")) {
        (Marker::Bullet, r)
    } else {
        let digits = t.chars().take_while(char::is_ascii_digit).count();
        if digits == 0 || digits > 9 {
            return None;
        }
        let n: u32 = t.get(..digits)?.parse().ok()?;
        let after = t.get(digits..)?;
        let r = after.strip_prefix(". ").or_else(|| after.strip_prefix(") "))?;
        (Marker::Number(n), r)
    };
    if marker == Marker::Bullet {
        if let Some(r) = rest.strip_prefix("[ ] ") {
            return Some((depth, Marker::Task(false), r));
        }
        if let Some(r) = rest.strip_prefix("[x] ").or_else(|| rest.strip_prefix("[X] ")) {
            return Some((depth, Marker::Task(true), r));
        }
    }
    Some((depth, marker, rest))
}

fn table_cells(line: &str) -> Vec<&str> {
    let t = line.trim();
    let t = t.strip_prefix('|').unwrap_or(t);
    let t = t.strip_suffix('|').unwrap_or(t);
    t.split('|').map(str::trim).collect()
}

fn separator_row(line: &str) -> bool {
    let cells = table_cells(line);
    line.contains('-')
        && !cells.is_empty()
        && cells.iter().all(|c| !c.is_empty() && c.chars().all(|ch| matches!(ch, '-' | ':' | ' ')) && c.contains('-'))
}

/// Blocks of a document, and where parsing may resume once more text is appended to it.
pub(super) struct Doc {
    pub blocks: Vec<MdBlock>,
    /// `blocks[..stable]` stay the same whatever text is appended.
    pub stable: usize,
    /// Byte offset of the line after the last stable block: parsing the appended text from here
    /// gives the blocks after `stable`.
    pub resume: usize,
}

/// Split a document into blocks.
pub fn parse(md: &str) -> Vec<MdBlock> {
    parse_doc(md, false).blocks
}

fn flush(para: &mut Vec<&str>, out: &mut Vec<MdBlock>, ends: &mut Vec<usize>, end: usize, repair: bool) {
    if !para.is_empty() {
        out.push(MdBlock::Paragraph(inline_with(&join_lines(para), repair)));
        ends.push(end);
        para.clear();
    }
}

/// [`parse`], also saying which blocks are final.
///
/// Each line's meaning depends on the line itself, the line after it (a table's separator) and
/// the open block. So a block is final once the line that ended it and the one after that are
/// complete (followed by a newline): appended text only changes the last, incomplete line and
/// adds lines. Every block starts with no open paragraph, so parsing from the line after a block
/// gives what a full parse gives.
pub(super) fn parse_doc(md: &str, repair: bool) -> Doc {
    let (md, rest) = if md.len() > MAX_BYTES {
        let mut end = MAX_BYTES;
        while !md.is_char_boundary(end) {
            end = end.saturating_sub(1);
        }
        (md.get(..end).unwrap_or_default(), md.get(end..))
    } else {
        (md, None)
    };
    // the lines of `str::lines`, with their byte offsets
    let mut lines: Vec<&str> = Vec::new();
    let mut starts: Vec<usize> = Vec::new();
    let mut complete = 0usize;
    let mut at = 0usize;
    for piece in md.split_inclusive('\n') {
        starts.push(at);
        at += piece.len();
        lines.push(match piece.strip_suffix('\n') {
            Some(l) => {
                complete += 1;
                l.strip_suffix('\r').unwrap_or(l)
            }
            None => piece,
        });
    }
    let mut out = Vec::new();
    // per block: the index of the line after it
    let mut ends: Vec<usize> = Vec::new();
    let mut para: Vec<&str> = Vec::new();
    let mut para_end = 0usize;
    let mut i = 0;
    while let Some(&line) = lines.get(i) {
        i += 1;
        if line.trim().is_empty() {
            flush(&mut para, &mut out, &mut ends, para_end, false);
            continue;
        }
        if let Some((f, lang)) = fence(line) {
            flush(&mut para, &mut out, &mut ends, para_end, false);
            let mut body: Vec<&str> = Vec::new();
            let mut closed = false;
            while let Some(&l) = lines.get(i) {
                i += 1;
                if l.trim_start().starts_with(f) && l.trim().trim_start_matches(f.chars().next().unwrap_or('`')).is_empty() {
                    closed = true;
                    break;
                }
                body.push(l);
            }
            out.push(MdBlock::Code { lang: lang.to_string(), text: body.join("\n"), closed });
            // an unclosed fence takes whatever comes next
            ends.push(if closed { i } else { usize::MAX });
            continue;
        }
        if let Some((level, text)) = heading(line) {
            flush(&mut para, &mut out, &mut ends, para_end, false);
            out.push(MdBlock::Heading { level, spans: inline_with(text, repair && i >= lines.len()) });
            ends.push(i);
            continue;
        }
        if is_rule(line) && para.is_empty() {
            out.push(MdBlock::Rule);
            ends.push(i);
            continue;
        }
        if let Some(q) = line.trim_start().strip_prefix('>') {
            flush(&mut para, &mut out, &mut ends, para_end, false);
            let mut text = vec![q.trim_start()];
            while let Some(q) = lines.get(i).and_then(|l| l.trim_start().strip_prefix('>')) {
                text.push(q.trim_start());
                i += 1;
            }
            out.push(MdBlock::Quote(inline_with(&join_lines(&text), repair && i >= lines.len())));
            ends.push(i);
            continue;
        }
        if let Some((depth, marker, text)) = item(line).filter(|_| !is_rule(line)) {
            flush(&mut para, &mut out, &mut ends, para_end, false);
            let mut text = vec![text];
            // continuation lines: indented, not a new item, not blank
            while let Some(&l) = lines.get(i) {
                if l.trim().is_empty() || item(l).is_some() || !l.starts_with(' ') || fence(l).is_some() {
                    break;
                }
                text.push(l.trim());
                i += 1;
            }
            out.push(MdBlock::Item { depth, marker, spans: inline_with(&join_lines(&text), repair && i >= lines.len()) });
            ends.push(i);
            continue;
        }
        if line.contains('|') && lines.get(i).is_some_and(|l| separator_row(l)) {
            flush(&mut para, &mut out, &mut ends, para_end, false);
            let header: Vec<Vec<Span>> = table_cells(line).into_iter().map(inline).collect();
            i += 1;
            let mut rows = Vec::new();
            while let Some(&l) = lines.get(i) {
                if !l.contains('|') || l.trim().is_empty() {
                    break;
                }
                rows.push(table_cells(l).into_iter().map(inline).collect());
                i += 1;
            }
            out.push(MdBlock::Table { header, rows });
            ends.push(i);
            continue;
        }
        para.push(line);
        para_end = i;
    }
    flush(&mut para, &mut out, &mut ends, para_end, repair);
    if let Some(r) = rest {
        out.push(MdBlock::Paragraph(vec![Span { text: r.to_string(), ..Span::default() }]));
        // past the size cap nothing is incremental
        return Doc { blocks: out, stable: 0, resume: 0 };
    }
    let stable = ends.iter().take_while(|e| e.saturating_add(2) <= complete).count();
    let resume = match stable.checked_sub(1).and_then(|b| ends.get(b)) {
        Some(&line) => starts.get(line).copied().unwrap_or(md.len()),
        None => 0,
    };
    Doc { blocks: out, stable, resume }
}

/// Lines of one paragraph: a line ending in two spaces or `\` breaks, others join with a space.
fn join_lines(lines: &[&str]) -> String {
    let mut s = String::new();
    for (n, l) in lines.iter().enumerate() {
        let hard = l.ends_with("  ") || l.ends_with('\\');
        let t = l.trim().trim_end_matches('\\');
        s.push_str(t);
        if n + 1 < lines.len() {
            s.push(if hard { '\n' } else { ' ' });
        }
    }
    s
}

/// The styled runs of one line of Markdown.
pub fn inline(s: &str) -> Vec<Span> {
    inline_with(s, false)
}

/// Where `pat` next starts at or after `from`, remembering the answer: `from` only grows, so the
/// text is scanned once however many openers ask (linear, not quadratic, on long paragraphs).
fn next_at(cache: &mut Option<Option<usize>>, s: &str, from: usize, pat: &str) -> Option<usize> {
    match *cache {
        Some(Some(p)) if p >= from => Some(p),
        Some(None) => None,
        _ => {
            let found = s.get(from..).and_then(|r| r.find(pat)).map(|p| p + from);
            *cache = Some(found);
            found
        }
    }
}

/// [`inline`]; with `repair` (the tail of a reply still streaming in) a marker with no closer yet
/// styles the rest as if it were closed, and a marker at the very end is hidden, so markers never
/// flash. A link whose URL has not arrived is styled as a link with an empty URL (not opened).
pub(super) fn inline_with(s: &str, repair: bool) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::new();
    let mut cur = Span::default();
    let push = |out: &mut Vec<Span>, cur: &mut Span| {
        if !cur.text.is_empty() {
            let next = Span { text: String::new(), ..cur.clone() };
            out.push(std::mem::replace(cur, next));
        }
    };
    // where each closer last starts: "is there one after here" is a comparison, not a scan
    let last_double = [s.rfind("**"), s.rfind("__"), s.rfind("~~")];
    let last_single = [s.rfind('*'), s.rfind('_')];
    // bit k set: no run of k + 1 backticks after the current position
    let mut no_ticks = 0u64;
    let (mut mid_at, mut close_at, mut bracket_at) = (None, None, None);
    let mut i = 0;
    while let Some(c) = s.get(i..).and_then(|r| r.chars().next()) {
        let after = s.get(i + c.len_utf8()..).unwrap_or_default();
        let next = after.chars().next();
        let prev = s.get(..i).and_then(|p| p.chars().next_back());
        match c {
            '\\' if next.is_some_and(|n| n.is_ascii_punctuation()) => {
                cur.text.push(next.unwrap_or('\\'));
                i += 2;
            }
            '`' => {
                let ticks = after.bytes().take_while(|b| *b == b'`').count() + 1;
                let start = i + ticks;
                let delim = s.get(i..start).unwrap_or("`");
                let bit = u32::try_from(ticks - 1).ok().and_then(|k| 1u64.checked_shl(k)).unwrap_or(0);
                let tail = s.get(start..).unwrap_or_default();
                let end = if bit == 0 || no_ticks & bit != 0 { None } else { tail.find(delim) };
                if end.is_none() {
                    no_ticks |= bit;
                }
                match end {
                    Some(end) => {
                        push(&mut out, &mut cur);
                        let code = tail.get(..end).unwrap_or_default();
                        let code = if code.starts_with(' ') && code.ends_with(' ') && code.len() > 1 {
                            code.get(1..code.len() - 1).unwrap_or(code)
                        } else {
                            code
                        };
                        out.push(Span { text: code.to_string(), code: true, link: cur.link.clone(), ..Span::default() });
                        i = start + end + ticks;
                    }
                    None if repair => {
                        // still streaming: the rest is code
                        push(&mut out, &mut cur);
                        if !tail.is_empty() {
                            out.push(Span { text: tail.to_string(), code: true, link: cur.link.clone(), ..Span::default() });
                        }
                        i = s.len();
                    }
                    None => {
                        cur.text.push_str(delim);
                        i = start;
                    }
                }
            }
            '*' | '_' | '~' if next == Some(c) => {
                let k = match c {
                    '*' => 0,
                    '_' => 1,
                    _ => 2,
                };
                let is_strike = c == '~';
                let on = if is_strike { cur.strike } else { cur.bold };
                let rest = s.get(i + 2..).unwrap_or_default();
                let closer = last_double.get(k).copied().flatten().is_some_and(|p| p >= i + 2);
                let word = rest.chars().next().is_some_and(|n| !n.is_whitespace());
                if on || (closer || repair) && word {
                    push(&mut out, &mut cur);
                    if is_strike {
                        cur.strike = !cur.strike;
                    } else {
                        cur.bold = !cur.bold;
                    }
                } else if !(repair && rest.is_empty()) {
                    cur.text.push(c);
                    cur.text.push(c);
                }
                i += 2;
            }
            '*' | '_' => {
                let word_inside = c == '_' && prev.is_some_and(char::is_alphanumeric) && next.is_some_and(char::is_alphanumeric);
                let closer = last_single.get(usize::from(c == '_')).copied().flatten().is_some_and(|p| p > i);
                let opens = !cur.italic
                    && next.is_some_and(|n| !n.is_whitespace())
                    && (closer || repair)
                    && !(c == '_' && prev.is_some_and(char::is_alphanumeric));
                let closes = cur.italic && prev.is_some_and(|p| !p.is_whitespace());
                if !word_inside && (opens || closes) {
                    push(&mut out, &mut cur);
                    cur.italic = !cur.italic;
                } else if !(repair && next.is_none()) {
                    cur.text.push(c);
                }
                i += 1;
            }
            '[' if cur.link.is_none() => {
                // [text](url)
                let base = i + 1;
                let mid = next_at(&mut mid_at, s, base, "](");
                let close = mid.and_then(|m| next_at(&mut close_at, s, m + 2, ")"));
                let parts = |m: usize, end: usize| Some((s.get(base..m)?, s.get(m + 2..end)?.trim()));
                let parsed = mid.zip(close).and_then(|(m, c)| {
                    let (text, url) = parts(m, c)?;
                    (!text.contains('\n') && !url.contains(' ')).then(|| (text, url.to_string(), c + 1))
                });
                // still streaming: `[text`, `[text]` at the end, or `[text](partial` is drawn as a link
                let pending = || {
                    if !repair {
                        return None;
                    }
                    let text = match mid {
                        Some(m) if close.is_none() => {
                            let url = s.get(m + 2..).unwrap_or_default();
                            (!url.contains(char::is_whitespace)).then(|| s.get(base..m))?
                        }
                        Some(_) => None,
                        None => match next_at(&mut bracket_at, s, base, "]") {
                            None => s.get(base..),
                            Some(b) if b + 1 == s.len() => s.get(base..b),
                            Some(_) => None,
                        },
                    }?;
                    (!text.contains('\n')).then(|| (text, String::new(), s.len()))
                };
                match parsed.or_else(pending) {
                    Some((text, url, end)) => {
                        push(&mut out, &mut cur);
                        for mut sp in inline_with(text, repair && url.is_empty()) {
                            sp.link = Some(url.clone());
                            sp.bold |= cur.bold;
                            sp.italic |= cur.italic;
                            out.push(sp);
                        }
                        i = end;
                    }
                    None => {
                        cur.text.push('[');
                        i += 1;
                    }
                }
            }
            'h' if cur.link.is_none()
                && prev.is_none_or(|p| p.is_whitespace() || p == '(')
                && s.get(i..).is_some_and(|r| r.starts_with("https://") || r.starts_with("http://")) =>
            {
                let r = s.get(i..).unwrap_or_default();
                let mut url = r.get(..r.find(char::is_whitespace).unwrap_or(r.len())).unwrap_or_default();
                url = url.trim_end_matches(['.', ',', ')', ';', ':', '!', '?']);
                push(&mut out, &mut cur);
                out.push(Span { text: url.to_string(), link: Some(url.to_string()), bold: cur.bold, italic: cur.italic, ..Span::default() });
                i += url.len().max(1);
            }
            _ => {
                cur.text.push(c);
                i += c.len_utf8();
            }
        }
    }
    push(&mut out, &mut cur);
    out
}
