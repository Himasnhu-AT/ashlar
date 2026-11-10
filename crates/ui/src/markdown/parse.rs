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
    parse_doc(md).blocks
}

fn flush(para: &mut Vec<&str>, out: &mut Vec<MdBlock>, ends: &mut Vec<usize>, end: usize) {
    if !para.is_empty() {
        out.push(MdBlock::Paragraph(inline(&join_lines(para))));
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
pub(super) fn parse_doc(md: &str) -> Doc {
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
            flush(&mut para, &mut out, &mut ends, para_end);
            continue;
        }
        if let Some((f, lang)) = fence(line) {
            flush(&mut para, &mut out, &mut ends, para_end);
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
            flush(&mut para, &mut out, &mut ends, para_end);
            out.push(MdBlock::Heading { level, spans: inline(text) });
            ends.push(i);
            continue;
        }
        if is_rule(line) && para.is_empty() {
            out.push(MdBlock::Rule);
            ends.push(i);
            continue;
        }
        if let Some(q) = line.trim_start().strip_prefix('>') {
            flush(&mut para, &mut out, &mut ends, para_end);
            let mut text = vec![q.trim_start()];
            while let Some(q) = lines.get(i).and_then(|l| l.trim_start().strip_prefix('>')) {
                text.push(q.trim_start());
                i += 1;
            }
            out.push(MdBlock::Quote(inline(&join_lines(&text))));
            ends.push(i);
            continue;
        }
        if let Some((depth, marker, text)) = item(line).filter(|_| !is_rule(line)) {
            flush(&mut para, &mut out, &mut ends, para_end);
            let mut text = vec![text];
            // continuation lines: indented, not a new item, not blank
            while let Some(&l) = lines.get(i) {
                if l.trim().is_empty() || item(l).is_some() || !l.starts_with(' ') || fence(l).is_some() {
                    break;
                }
                text.push(l.trim());
                i += 1;
            }
            out.push(MdBlock::Item { depth, marker, spans: inline(&join_lines(&text)) });
            ends.push(i);
            continue;
        }
        if line.contains('|') && lines.get(i).is_some_and(|l| separator_row(l)) {
            flush(&mut para, &mut out, &mut ends, para_end);
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
    flush(&mut para, &mut out, &mut ends, para_end);
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
    let mut out: Vec<Span> = Vec::new();
    let mut cur = Span::default();
    let chars: Vec<char> = s.chars().collect();
    let rest = |i: usize| -> String { chars.get(i..).unwrap_or_default().iter().collect() };
    let push = |out: &mut Vec<Span>, cur: &mut Span| {
        if !cur.text.is_empty() {
            let next = Span { text: String::new(), ..cur.clone() };
            out.push(std::mem::replace(cur, next));
        }
    };
    let mut i = 0;
    while let Some(&c) = chars.get(i) {
        let next = chars.get(i + 1).copied();
        let prev = i.checked_sub(1).and_then(|p| chars.get(p)).copied();
        match c {
            '\\' if next.is_some_and(|n| n.is_ascii_punctuation()) => {
                cur.text.push(next.unwrap_or('\\'));
                i += 2;
            }
            '`' => {
                let ticks = chars.get(i..).unwrap_or_default().iter().take_while(|c| **c == '`').count();
                let delim: String = "`".repeat(ticks);
                let after = rest(i + ticks);
                match after.find(&delim) {
                    Some(end) => {
                        push(&mut out, &mut cur);
                        let code = after.get(..end).unwrap_or_default();
                        let code = if code.starts_with(' ') && code.ends_with(' ') && code.len() > 1 {
                            code.get(1..code.len() - 1).unwrap_or(code)
                        } else {
                            code
                        };
                        out.push(Span { text: code.to_string(), code: true, link: cur.link.clone(), ..Span::default() });
                        i += ticks + after.get(..end).map_or(0, |x| x.chars().count()) + ticks;
                    }
                    None => {
                        cur.text.push_str(&delim);
                        i += ticks;
                    }
                }
            }
            '*' | '_' | '~' if next == Some(c) => {
                let delim: String = [c, c].iter().collect();
                let is_strike = c == '~';
                let on = if is_strike { cur.strike } else { cur.bold };
                if on || rest(i + 2).contains(&delim) && chars.get(i + 2).is_some_and(|n| !n.is_whitespace()) {
                    push(&mut out, &mut cur);
                    if is_strike {
                        cur.strike = !cur.strike;
                    } else {
                        cur.bold = !cur.bold;
                    }
                } else {
                    cur.text.push_str(&delim);
                }
                i += 2;
            }
            '*' | '_' => {
                let word_inside = c == '_' && prev.is_some_and(char::is_alphanumeric) && next.is_some_and(char::is_alphanumeric);
                let opens = !cur.italic
                    && next.is_some_and(|n| !n.is_whitespace())
                    && rest(i + 1).contains(c)
                    && !(c == '_' && prev.is_some_and(char::is_alphanumeric));
                let closes = cur.italic && prev.is_some_and(|p| !p.is_whitespace());
                if !word_inside && (opens || closes) {
                    push(&mut out, &mut cur);
                    cur.italic = !cur.italic;
                } else {
                    cur.text.push(c);
                }
                i += 1;
            }
            '[' if cur.link.is_none() => {
                // [text](url)
                let r = rest(i + 1);
                let parsed = r.find("](").and_then(|mid| {
                    let text = r.get(..mid)?;
                    let after = r.get(mid + 2..)?;
                    let end = after.find(')')?;
                    let url = after.get(..end)?.trim();
                    (!text.contains('\n') && !url.contains(' '))
                        .then(|| (text.to_string(), url.to_string(), text.chars().count() + 2 + url.chars().count() + 1))
                });
                match parsed {
                    Some((text, url, consumed)) => {
                        push(&mut out, &mut cur);
                        for mut s in inline(&text) {
                            s.link = Some(url.clone());
                            s.bold |= cur.bold;
                            s.italic |= cur.italic;
                            out.push(s);
                        }
                        i += 1 + consumed;
                    }
                    None => {
                        cur.text.push('[');
                        i += 1;
                    }
                }
            }
            'h' if cur.link.is_none()
                && prev.is_none_or(|p| p.is_whitespace() || p == '(')
                && (rest(i).starts_with("https://") || rest(i).starts_with("http://")) =>
            {
                let r = rest(i);
                let mut url: String = r.chars().take_while(|c| !c.is_whitespace()).collect();
                while url.ends_with(['.', ',', ')', ';', ':', '!', '?']) {
                    url.pop();
                }
                push(&mut out, &mut cur);
                let n = url.chars().count();
                out.push(Span { text: url.clone(), link: Some(url), bold: cur.bold, italic: cur.italic, ..Span::default() });
                i += n.max(1);
            }
            _ => {
                cur.text.push(c);
                i += 1;
            }
        }
    }
    push(&mut out, &mut cur);
    out
}
