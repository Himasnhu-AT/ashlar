//! Markdown for chat replies and short documents: headings, paragraphs, bullet, numbered and
//! task lists, block quotes, rules, tables and fenced code blocks (with a Copy button), and inline
//! code, bold, italic, strike-through and links.
//!
//! [`parse`] is forgiving and made for text that is still streaming in: an unclosed code fence
//! runs to the end, an unclosed `**` or `` ` `` is shown as typed, so a reply does not jump as it
//! grows. [`show`] draws the blocks with a [`MarkdownStyle`] the app fills from its theme.
//! Paragraphs are selectable; a link opens in the browser (`http`, `https` and `mailto` only).
//!
//! Automation ids (under the `id` passed to [`show`]): `<id>.code.<n>` for each code block and
//! `<id>.code.<n>.copy` for its Copy button, `<id>.link.<n>` for each link.

use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, CornerRadius, FontFamily, FontId, Frame, Margin, Sense, Stroke, Ui};

use crate::automation::{self, Details, Role};

/// A run of text with one style.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    pub strike: bool,
    pub link: Option<String>,
}

/// A list item's marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Marker {
    Bullet,
    Number(u32),
    Task(bool),
}

/// One block of a document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MdBlock {
    Heading {
        level: u8,
        spans: Vec<Span>,
    },
    Paragraph(Vec<Span>),
    Item {
        depth: u8,
        marker: Marker,
        spans: Vec<Span>,
    },
    Quote(Vec<Span>),
    /// `closed`: the closing fence has arrived (false while a reply is still streaming).
    Code {
        lang: String,
        text: String,
        closed: bool,
    },
    Rule,
    Table {
        header: Vec<Vec<Span>>,
        rows: Vec<Vec<Vec<Span>>>,
    },
}

/// Longest document parsed (the rest is shown as one plain paragraph).
pub const MAX_BYTES: usize = 2 * 1024 * 1024;

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

/// Split a document into blocks.
pub fn parse(md: &str) -> Vec<MdBlock> {
    let (md, rest) = if md.len() > MAX_BYTES {
        let mut end = MAX_BYTES;
        while !md.is_char_boundary(end) {
            end = end.saturating_sub(1);
        }
        (md.get(..end).unwrap_or_default(), md.get(end..))
    } else {
        (md, None)
    };
    let lines: Vec<&str> = md.lines().collect();
    let mut out = Vec::new();
    let mut para: Vec<&str> = Vec::new();
    let flush = |para: &mut Vec<&str>, out: &mut Vec<MdBlock>| {
        if !para.is_empty() {
            out.push(MdBlock::Paragraph(inline(&join_lines(para))));
            para.clear();
        }
    };
    let mut i = 0;
    while let Some(&line) = lines.get(i) {
        i += 1;
        if line.trim().is_empty() {
            flush(&mut para, &mut out);
            continue;
        }
        if let Some((f, lang)) = fence(line) {
            flush(&mut para, &mut out);
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
            continue;
        }
        if let Some((level, text)) = heading(line) {
            flush(&mut para, &mut out);
            out.push(MdBlock::Heading { level, spans: inline(text) });
            continue;
        }
        if is_rule(line) && para.is_empty() {
            out.push(MdBlock::Rule);
            continue;
        }
        if let Some(q) = line.trim_start().strip_prefix('>') {
            flush(&mut para, &mut out);
            let mut text = vec![q.trim_start()];
            while let Some(q) = lines.get(i).and_then(|l| l.trim_start().strip_prefix('>')) {
                text.push(q.trim_start());
                i += 1;
            }
            out.push(MdBlock::Quote(inline(&join_lines(&text))));
            continue;
        }
        if let Some((depth, marker, text)) = item(line).filter(|_| !is_rule(line)) {
            flush(&mut para, &mut out);
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
            continue;
        }
        if line.contains('|') && lines.get(i).is_some_and(|l| separator_row(l)) {
            flush(&mut para, &mut out);
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
            continue;
        }
        para.push(line);
    }
    flush(&mut para, &mut out);
    if let Some(r) = rest {
        out.push(MdBlock::Paragraph(vec![Span { text: r.to_string(), ..Span::default() }]));
    }
    out
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

/// The plain text of runs (what Copy puts on the clipboard for a paragraph).
pub fn plain(spans: &[Span]) -> String {
    spans.iter().map(|s| s.text.as_str()).collect()
}

/// Colours and fonts, from the app's theme.
#[derive(Debug, Clone, PartialEq)]
pub struct MarkdownStyle {
    pub text: Color32,
    /// Bold text and headings.
    pub strong: Color32,
    /// Quotes, list markers, a code block's language.
    pub weak: Color32,
    pub link: Color32,
    pub code_text: Color32,
    pub code_bg: Color32,
    pub code_border: Color32,
    pub rule: Color32,
    pub body: FontId,
    pub mono: FontId,
    /// A bold face, when the app has one (otherwise bold is the strong colour).
    pub bold_family: Option<FontFamily>,
    /// Heading sizes for `#`, `##` and `###` (deeper levels use the last).
    pub heading_sizes: [f32; 3],
    pub corner: CornerRadius,
}

impl MarkdownStyle {
    /// From egui's visuals (an app with its own tokens sets the fields itself).
    pub fn from_visuals(v: &egui::Visuals, body_size: f32) -> Self {
        Self {
            text: v.text_color(),
            strong: v.strong_text_color(),
            weak: v.weak_text_color(),
            link: v.hyperlink_color,
            code_text: v.text_color(),
            code_bg: v.extreme_bg_color,
            code_border: v.widgets.noninteractive.bg_stroke.color,
            rule: v.widgets.noninteractive.bg_stroke.color,
            body: FontId::proportional(body_size),
            mono: FontId::monospace(body_size * 0.92),
            bold_family: None,
            heading_sizes: [body_size * 1.45, body_size * 1.25, body_size * 1.1],
            corner: CornerRadius::same(4),
        }
    }

    fn format(&self, s: &Span, font: &FontId, base: Color32) -> TextFormat {
        let mut f = TextFormat { font_id: font.clone(), color: base, ..Default::default() };
        if s.code {
            f.font_id = FontId::new(self.mono.size * font.size / self.body.size.max(1.0), self.mono.family.clone());
            f.color = self.code_text;
            f.background = self.code_bg;
        }
        if s.bold {
            f.color = self.strong;
            if let Some(fam) = &self.bold_family {
                f.font_id.family = fam.clone();
            }
        }
        if s.italic {
            f.italics = true;
        }
        if s.strike {
            f.strikethrough = Stroke::new(1.0, f.color);
        }
        if s.link.is_some() {
            f.color = self.link;
            f.underline = Stroke::new(1.0, self.link);
        }
        f
    }
}

/// What the person did in [`show`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MarkdownResponse {
    /// The code block (counted from 0) whose Copy was pressed.
    pub copied: Option<usize>,
    /// The link that was opened.
    pub opened: Option<String>,
}

fn safe_link(url: &str) -> bool {
    let u = url.trim().to_ascii_lowercase();
    u.starts_with("https://") || u.starts_with("http://") || u.starts_with("mailto:")
}

/// Draw `md` at the full available width. `id` prefixes the automation ids.
pub fn show(ui: &mut Ui, id: &str, md: &str, style: &MarkdownStyle) -> MarkdownResponse {
    show_blocks(ui, id, &parse(md), style)
}

/// [`show`] with blocks parsed already (cache them while a long text doesn't change).
pub fn show_blocks(ui: &mut Ui, id: &str, blocks: &[MdBlock], style: &MarkdownStyle) -> MarkdownResponse {
    let mut resp = MarkdownResponse::default();
    let mut code_n = 0usize;
    let mut link_n = 0usize;
    let mut table_n = 0usize;
    let gap = style.body.size * 0.45;
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = gap;
        for b in blocks {
            match b {
                MdBlock::Heading { level, spans } => {
                    let size = style
                        .heading_sizes
                        .get(usize::from(level.saturating_sub(1)))
                        .or(style.heading_sizes.last())
                        .copied()
                        .unwrap_or(style.body.size);
                    let font = FontId::new(size, style.bold_family.clone().unwrap_or_else(|| style.body.family.clone()));
                    let bold: Vec<Span> = spans.iter().map(|s| Span { bold: true, ..s.clone() }).collect();
                    ui.add_space(gap * 0.5);
                    paragraph(ui, id, &bold, style, &font, style.strong, &mut link_n, &mut resp);
                }
                MdBlock::Paragraph(spans) => paragraph(ui, id, spans, style, &style.body, style.text, &mut link_n, &mut resp),
                MdBlock::Quote(spans) => {
                    let r = ui.horizontal_top(|ui| {
                        ui.add_space(10.0);
                        paragraph(ui, id, spans, style, &style.body, style.weak, &mut link_n, &mut resp);
                    });
                    let rect = r.response.rect;
                    ui.painter().line_segment(
                        [rect.left_top() + egui::vec2(3.0, 0.0), rect.left_bottom() + egui::vec2(3.0, 0.0)],
                        Stroke::new(2.0, style.rule),
                    );
                }
                MdBlock::Item { depth, marker, spans } => {
                    ui.horizontal_top(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        ui.add_space(f32::from(*depth) * style.body.size * 1.2);
                        let m = match marker {
                            Marker::Bullet => if *depth == 0 { "•" } else { "◦" }.to_string(),
                            Marker::Number(n) => format!("{n}."),
                            Marker::Task(true) => "☑".into(),
                            Marker::Task(false) => "☐".into(),
                        };
                        let w = style.body.size * 1.1;
                        let (rect, _) = ui.allocate_exact_size(egui::vec2(w, style.body.size * 1.3), Sense::hover());
                        ui.painter().text(rect.right_top(), egui::Align2::RIGHT_TOP, m, style.body.clone(), style.weak);
                        paragraph(ui, id, spans, style, &style.body, style.text, &mut link_n, &mut resp);
                    });
                }
                MdBlock::Rule => {
                    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), gap), Sense::hover());
                    ui.painter().hline(rect.x_range(), rect.center().y, Stroke::new(1.0, style.rule));
                }
                MdBlock::Code { lang, text, .. } => {
                    if code_block(ui, id, code_n, lang, text, style) {
                        resp.copied = Some(code_n);
                    }
                    code_n += 1;
                }
                MdBlock::Table { header, rows } => {
                    table(ui, id, table_n, header, rows, style);
                    table_n += 1;
                }
            }
        }
    });
    resp
}

#[allow(clippy::too_many_arguments)]
fn paragraph(
    ui: &mut Ui,
    id: &str,
    spans: &[Span],
    style: &MarkdownStyle,
    font: &FontId,
    base: Color32,
    link_n: &mut usize,
    resp: &mut MarkdownResponse,
) {
    let mut job = LayoutJob::default();
    let mut ranges: Vec<(std::ops::Range<usize>, String)> = Vec::new();
    let mut chars = 0usize;
    for s in spans {
        let n = s.text.chars().count();
        if let Some(l) = &s.link {
            ranges.push((chars..chars + n, l.clone()));
        }
        chars += n;
        job.append(&s.text, 0.0, style.format(s, font, base));
    }
    job.wrap.max_width = ui.available_width().max(10.0);
    if ranges.is_empty() {
        ui.add(egui::Label::new(job).wrap().selectable(true));
        return;
    }
    // with links: lay out, paint, and map a click to the link under the pointer
    let (pos, galley, r) = egui::Label::new(job).wrap().selectable(false).sense(Sense::click()).layout_in_ui(ui);
    let hovered_link = r.hover_pos().and_then(|p| {
        let c = galley.cursor_from_pos(p - pos).index.0;
        ranges.iter().find(|(range, _)| range.contains(&c)).map(|(_, u)| u.clone())
    });
    if hovered_link.is_some() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if r.clicked()
        && let Some(url) = hovered_link.clone()
        && safe_link(&url)
    {
        ui.ctx().open_url(egui::OpenUrl::new_tab(&url));
        resp.opened = Some(url);
    }
    ui.painter().galley(pos, galley.clone(), base);
    for (range, url) in &ranges {
        // a rect for each link (its first row), for automation
        let a = galley.pos_from_cursor(egui::text::CCursor::new(range.start));
        let b = galley.pos_from_cursor(egui::text::CCursor::new(range.end));
        let rect = if (a.min.y - b.min.y).abs() < 1.0 {
            egui::Rect::from_min_max(a.min, b.max)
        } else {
            egui::Rect::from_min_max(a.min, egui::pos2(galley.rect.right(), a.max.y))
        };
        automation::register_fmt(
            ui,
            rect.translate(pos.to_vec2()),
            format_args!("{id}.link.{}", *link_n),
            url,
            Role::Button,
            true,
            Details { value: Some(url), selected: None },
        );
        *link_n += 1;
    }
}

/// A code block; true when its Copy was pressed.
fn code_block(ui: &mut Ui, id: &str, n: usize, lang: &str, text: &str, style: &MarkdownStyle) -> bool {
    let mut copied = false;
    let r = Frame::new()
        .fill(style.code_bg)
        .stroke(Stroke::new(1.0, style.code_border))
        .corner_radius(style.corner)
        .inner_margin(Margin::symmetric(8, 6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let small = FontId::new(style.body.size * 0.85, style.body.family.clone());
                ui.label(egui::RichText::new(if lang.is_empty() { "code" } else { lang }).font(small.clone()).color(style.weak));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let key = egui::Id::new((id, "md-copied", n));
                    let recently = ui.ctx().data(|d| d.get_temp::<f64>(key)).is_some_and(|t| ui.input(|i| i.time) - t < 1.5);
                    let b = ui.add(
                        egui::Button::new(egui::RichText::new(if recently { "Copied" } else { "Copy" }).font(small).color(style.weak)).frame(false),
                    );
                    automation::register(ui, &b, &format!("{id}.code.{n}.copy"), "Copy", Role::Button);
                    if b.clicked() {
                        ui.ctx().copy_text(text.to_string());
                        let now = ui.input(|i| i.time);
                        ui.ctx().data_mut(|d| d.insert_temp(key, now));
                        ui.ctx().request_repaint_after(std::time::Duration::from_millis(1600));
                        copied = true;
                    }
                });
            });
            egui::ScrollArea::horizontal().id_salt((id, "md-code", n)).show(ui, |ui| {
                let mut job = LayoutJob::single_section(
                    text.to_string(),
                    TextFormat { font_id: style.mono.clone(), color: style.code_text, ..Default::default() },
                );
                job.wrap.max_width = f32::INFINITY;
                ui.add(egui::Label::new(job).extend().selectable(true));
            });
        });
    automation::register_fmt(
        ui,
        r.response.rect,
        format_args!("{id}.code.{n}"),
        lang,
        Role::Other,
        true,
        Details { value: Some(text.get(..text.len().min(200)).unwrap_or_default()), selected: None },
    );
    copied
}

fn table(ui: &mut Ui, id: &str, n: usize, header: &[Vec<Span>], rows: &[Vec<Vec<Span>>], style: &MarkdownStyle) {
    let cols = header.len().max(rows.iter().map(Vec::len).max().unwrap_or(0)).clamp(1, 24);
    egui::ScrollArea::horizontal().id_salt((id, "md-table", n)).show(ui, |ui| {
        egui::Grid::new((id, "md-grid", n)).striped(true).spacing(egui::vec2(14.0, 4.0)).show(ui, |ui| {
            for c in 0..cols {
                let spans: Vec<Span> = header.get(c).map(|s| s.iter().map(|x| Span { bold: true, ..x.clone() }).collect()).unwrap_or_default();
                cell(ui, &spans, style);
            }
            ui.end_row();
            for row in rows.iter().take(500) {
                for c in 0..cols {
                    cell(ui, row.get(c).map(Vec::as_slice).unwrap_or_default(), style);
                }
                ui.end_row();
            }
        });
    });
}

fn cell(ui: &mut Ui, spans: &[Span], style: &MarkdownStyle) {
    let mut job = LayoutJob::default();
    for s in spans {
        job.append(&s.text, 0.0, style.format(s, &style.body, style.text));
    }
    job.wrap.max_width = 320.0;
    ui.add(egui::Label::new(job).selectable(true));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(text: &str) -> Span {
        Span { text: text.into(), ..Span::default() }
    }

    #[test]
    fn blocks() {
        let md = "# Title\n\nSome *soft*\nwrapped text.\n\n- one\n  more\n- [x] done\n  1. sub\n\n```rust\nfn main() {}\n```\n> quoted\n> twice\n\n---\n| a | b |\n|---|:-:|\n| 1 | 2 |\n";
        let b = parse(md);
        assert_eq!(b.first(), Some(&MdBlock::Heading { level: 1, spans: vec![t("Title")] }));
        assert_eq!(
            b.get(1),
            Some(&MdBlock::Paragraph(vec![t("Some "), Span { text: "soft".into(), italic: true, ..Span::default() }, t(" wrapped text.")]))
        );
        assert_eq!(b.get(2), Some(&MdBlock::Item { depth: 0, marker: Marker::Bullet, spans: vec![t("one more")] }));
        assert_eq!(b.get(3), Some(&MdBlock::Item { depth: 0, marker: Marker::Task(true), spans: vec![t("done")] }));
        assert_eq!(b.get(4), Some(&MdBlock::Item { depth: 1, marker: Marker::Number(1), spans: vec![t("sub")] }));
        assert_eq!(b.get(5), Some(&MdBlock::Code { lang: "rust".into(), text: "fn main() {}".into(), closed: true }));
        assert_eq!(b.get(6), Some(&MdBlock::Quote(vec![t("quoted twice")])));
        assert_eq!(b.get(7), Some(&MdBlock::Rule));
        assert_eq!(b.get(8), Some(&MdBlock::Table { header: vec![vec![t("a")], vec![t("b")]], rows: vec![vec![vec![t("1")], vec![t("2")]]] }));
        assert_eq!(b.len(), 9);
    }

    #[test]
    fn streaming_text_never_jumps_back() {
        // an unclosed fence runs to the end; unclosed inline marks show as typed
        assert_eq!(parse("```py\nprint(1)"), vec![MdBlock::Code { lang: "py".into(), text: "print(1)".into(), closed: false }]);
        assert_eq!(inline("a **bol"), vec![t("a **bol")]);
        assert_eq!(inline("run `cargo"), vec![t("run `cargo")]);
        assert_eq!(inline("[half](page"), vec![t("[half](page")]);
    }

    #[test]
    fn inline_styles() {
        let s = inline("**bold** and `code` and ~~gone~~ and snake_case_name and \\*literal\\*");
        assert_eq!(s.first(), Some(&Span { text: "bold".into(), bold: true, ..Span::default() }));
        assert!(s.iter().any(|x| x.code && x.text == "code"));
        assert!(s.iter().any(|x| x.strike && x.text == "gone"));
        assert_eq!(plain(&s), "bold and code and gone and snake_case_name and *literal*");
        let l = inline("see [the docs](https://example.com/a) or https://example.com/b.");
        assert_eq!(l.get(1), Some(&Span { text: "the docs".into(), link: Some("https://example.com/a".into()), ..Span::default() }));
        assert_eq!(l.get(3).and_then(|s| s.link.as_deref()), Some("https://example.com/b"));
        assert_eq!(l.last(), Some(&t(".")));
        assert_eq!(inline("2 * 3 * 4"), vec![t("2 * 3 * 4")], "a lone star is not italic");
        assert!(!safe_link("javascript:alert(1)") && safe_link("https://a.b"));
    }

    #[test]
    fn hostile_input_does_not_panic() {
        for s in ["`", "``", "**", "*", "_", "[", "[]()", "](", "|", "|-|", "#", "####### x", "1.", "- ", "> ", "\\", "~~", "```", "\u{301}*a*"] {
            let _ = parse(s);
            let _ = inline(s);
        }
        let big = "*a ".repeat(50_000);
        assert!(!parse(&big).is_empty());
    }

    #[test]
    fn it_draws_and_copies_a_code_block() {
        let ctx = egui::Context::default();
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark(), 13.0);
        let md = "## Steps\n1. Open **Main**\n2. See [docs](https://example.com)\n\n```sh\necho hi\n```";
        let mut out = MarkdownResponse::default();
        for _ in 0..2 {
            let _ = crate::test_support::run(&ctx, egui::RawInput::default(), |ui| {
                out = show(ui, "md", md, &style);
            });
        }
        assert_eq!(out, MarkdownResponse::default());
        let ids: Vec<String> = automation::query(&ctx, "md.").into_iter().map(|e| e.id).collect();
        assert!(ids.contains(&"md.code.0".to_string()), "{ids:?}");
        assert!(ids.contains(&"md.code.0.copy".to_string()));
        assert!(ids.contains(&"md.link.0".to_string()));
    }
}
