//! Log lines as crash breadcrumbs, with anything that could name the user's things removed.

/// Remove what could name the user's things from a log line: anything that looks like a path
/// (`/`, `\`), a file name (`name.ext`) or quoted text becomes a placeholder.
pub fn redact(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        let close = match c {
            '"' => Some('"'),
            '`' => Some('`'),
            '“' => Some('”'),
            '‘' => Some('’'),
            '«' => Some('»'),
            '\'' if out.is_empty() || out.ends_with([' ', '(', '[', ':', '=']) => Some('\''),
            _ => None,
        };
        if let Some(close) = close {
            for d in chars.by_ref() {
                if d == close {
                    break;
                }
            }
            out.push_str("<text>");
            continue;
        }
        if c.is_whitespace() {
            out.push(c);
            continue;
        }
        let mut word = String::from(c);
        while let Some(&d) = chars.peek() {
            if d.is_whitespace() || matches!(d, '"' | '`' | '“' | '‘' | '«') || (d == '\'' && word.ends_with(['(', '[', ':', '='])) {
                break;
            }
            word.push(d);
            chars.next();
        }
        out.push_str(&redact_word(&word));
    }
    out
}

fn redact_word(word: &str) -> String {
    let core = word.trim_matches(|c: char| matches!(c, ',' | ';' | ')' | '(' | '[' | ']' | '{' | '}' | '<' | '>'));
    let core = core.strip_suffix(':').unwrap_or(core);
    let core = core.strip_suffix('.').unwrap_or(core);
    if core.contains(['/', '\\']) {
        return word.replacen(core, "<path>", 1);
    }
    let file_like = core.rsplit_once('.').is_some_and(|(name, ext)| {
        !name.is_empty()
            && !name.ends_with('.')
            && (1..=5).contains(&ext.len())
            && ext.chars().all(|c| c.is_ascii_alphanumeric())
            && ext.chars().any(|c| c.is_ascii_alphabetic())
            // `3.2s` is a number with a unit; `2024.mp4` a file
            && (name.chars().any(char::is_alphabetic) || ext.len() >= 3)
            && !core.contains("::")
    });
    if file_like { word.replacen(core, "<file>", 1) } else { word.to_string() }
}

/// The app's logger with the SDK's breadcrumb logger beside it (see
/// [`crate::Telemetry::wrap_logger`]).
pub(crate) struct Breadcrumbs {
    pub(crate) inner: Box<dyn log::Log>,
    pub(crate) crumbs: Box<dyn log::Log>,
}

impl log::Log for Breadcrumbs {
    fn enabled(&self, m: &log::Metadata<'_>) -> bool {
        self.inner.enabled(m)
    }

    fn log(&self, record: &log::Record<'_>) {
        if !self.inner.enabled(record.metadata()) {
            return;
        }
        self.inner.log(record);
        if self.crumbs.enabled(record.metadata()) {
            let line = redact(&record.args().to_string());
            self.crumbs.log(&log::Record::builder().level(record.level()).target(record.target()).args(format_args!("{line}")).build());
        }
    }

    fn flush(&self) {
        self.inner.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_lines_lose_paths_names_and_quotes() {
        assert_eq!(redact("can't open /Users/ann/Movies/My Trip.mov: denied"), "can't open <path> <file>: denied");
        assert_eq!(redact(r"read C:\Users\ann\a.wav failed"), "read <path> failed");
        assert_eq!(redact("imported holiday.mp4, 3 clips"), "imported <file>, 3 clips");
        assert_eq!(redact("clip \"Interview A\" offline"), "clip <text> offline");
        assert_eq!(redact("project `secret plan` saved"), "project <text> saved");
        assert_eq!(redact("name='Ann' x"), "name=<text> x");
        assert_eq!(redact("hardware decoding: Available { h264: true }"), "hardware decoding: Available { h264: true }");
        assert_eq!(redact("decoded 12.5 fps at core_engine::media in 3.2s"), "decoded 12.5 fps at core_engine::media in 3.2s");
        assert_eq!(redact("GPU error: wgpu v0.24 lost"), "GPU error: wgpu v0.24 lost");
    }
}
