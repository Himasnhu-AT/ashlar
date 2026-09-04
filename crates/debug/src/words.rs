//! Words: splitting a script line like a shell does (quotes, escapes, comments) and a small
//! argument parser (flags, options and positionals in any order).

/// Split a line into words: whitespace separates, `"…"` and `'…'` group (with `\"`, `\\`, `\n`,
/// `\t` inside double quotes), and an unquoted `#` at the start of a word begins a comment.
pub fn split(line: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            '#' if !in_word => break,
            '"' => {
                in_word = true;
                loop {
                    match chars.next() {
                        None => return Err("unterminated \" quote".into()),
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some('n') => cur.push('\n'),
                            Some('t') => cur.push('\t'),
                            Some(e) => cur.push(e),
                            None => return Err("unterminated \" quote".into()),
                        },
                        Some(x) => cur.push(x),
                    }
                }
            }
            '\'' => {
                in_word = true;
                loop {
                    match chars.next() {
                        None => return Err("unterminated ' quote".into()),
                        Some('\'') => break,
                        Some(x) => cur.push(x),
                    }
                }
            }
            c => {
                in_word = true;
                cur.push(c);
            }
        }
    }
    if in_word {
        words.push(cur);
    }
    Ok(words)
}

/// Quote a word for display when it needs it (the inverse of [`split`] for one word).
pub fn quote(w: &str) -> String {
    if !w.is_empty() && !w.chars().any(|c| c.is_whitespace() || matches!(c, '"' | '\'' | '\\' | '#')) {
        return w.to_string();
    }
    let mut s = String::from("\"");
    for c in w.chars() {
        match c {
            '"' => s.push_str("\\\""),
            '\\' => s.push_str("\\\\"),
            '\n' => s.push_str("\\n"),
            '\t' => s.push_str("\\t"),
            c => s.push(c),
        }
    }
    s.push('"');
    s
}

/// The arguments after the verb. Options are taken by name; what is left are positionals.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Args {
    items: Vec<String>,
    /// Items after a bare `--` are positionals even when they start with `-`.
    rest: Vec<String>,
}

fn is_option(a: &str) -> bool {
    // `-5` is a number (scroll by a negative amount), not an option.
    a.starts_with('-') && a.len() > 1 && !a.chars().nth(1).is_some_and(|c| c.is_ascii_digit())
}

impl Args {
    pub fn new(items: impl IntoIterator<Item = impl Into<String>>) -> Self {
        let mut all: Vec<String> = items.into_iter().map(Into::into).collect();
        let rest = match all.iter().position(|a| a == "--") {
            Some(i) => all.split_off(i).into_iter().skip(1).collect(),
            None => Vec::new(),
        };
        Self { items: all, rest }
    }

    /// Take a flag by any of its names (`-i`, `--interactive`).
    pub fn flag(&mut self, names: &[&str]) -> bool {
        let mut found = false;
        self.items.retain(|a| {
            let hit = names.contains(&a.as_str());
            found |= hit;
            !hit
        });
        found
    }

    /// Take `--name VALUE` or `--name=VALUE` (the last one wins).
    pub fn value(&mut self, names: &[&str]) -> Result<Option<String>, String> {
        let mut out = None;
        let mut i = 0;
        while i < self.items.len() {
            let a = self.items.get(i).cloned().unwrap_or_default();
            if let Some(v) = names.iter().find_map(|n| a.strip_prefix(&format!("{n}="))) {
                out = Some(v.to_string());
                self.items.remove(i);
            } else if names.contains(&a.as_str()) {
                self.items.remove(i);
                if i < self.items.len() {
                    out = Some(self.items.remove(i));
                } else {
                    return Err(format!("{a} needs a value"));
                }
            } else {
                i = i.saturating_add(1);
            }
        }
        Ok(out)
    }

    /// Take `--name N` as a number.
    pub fn number<T: std::str::FromStr>(&mut self, names: &[&str]) -> Result<Option<T>, String> {
        match self.value(names)? {
            Some(v) => v.parse().map(Some).map_err(|_| format!("{}: `{v}` is not a valid number", names.first().copied().unwrap_or("option"))),
            None => Ok(None),
        }
    }

    /// Take the next positional. Take options first, so an option's value is not mistaken for one.
    pub fn positional(&mut self) -> Option<String> {
        match self.items.iter().position(|a| !is_option(a)) {
            Some(i) => Some(self.items.remove(i)),
            None => (!self.rest.is_empty()).then(|| self.rest.remove(0)),
        }
    }

    /// Every remaining positional (including those after `--`).
    pub fn rest(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        while let Some(p) = self.positional() {
            out.push(p);
        }
        out
    }

    /// Whether anything (option or positional) is left.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty() && self.rest.is_empty()
    }

    /// Everything was taken; otherwise the first leftover is an error.
    pub fn finish(self) -> Result<(), String> {
        match self.items.first().or(self.rest.first()) {
            None => Ok(()),
            Some(a) if is_option(a) => Err(format!("unknown option {a}")),
            Some(a) => Err(format!("unexpected argument `{a}`")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_like_a_shell() {
        assert_eq!(split(r#"fill @e3 "hello world""#).unwrap(), ["fill", "@e3", "hello world"]);
        assert_eq!(split(r#"type 'it''s' "a\"b\\c\n""#).unwrap(), ["type", "its", "a\"b\\c\n"]);
        assert_eq!(split("  click e3   # a comment").unwrap(), ["click", "e3"]);
        assert_eq!(split("# only a comment").unwrap(), Vec::<String>::new());
        assert_eq!(split(r#"fill e1 "" x"#).unwrap(), ["fill", "e1", "", "x"]);
        assert_eq!(split("text=a#b").unwrap(), ["text=a#b"]);
        assert!(split(r#"fill e1 "open"#).is_err());
        assert!(split("fill e1 'open").is_err());
    }

    #[test]
    fn quote_round_trips() {
        for w in ["plain", "two words", "", "a\"b", "back\\slash", "#hash", "tab\there"] {
            assert_eq!(split(&quote(w)).unwrap(), [w], "{w}");
        }
    }

    #[test]
    fn options_in_any_order() {
        let mut a = Args::new(["@e3", "--count=2", "-i", "--button", "secondary", "--", "--literal"]);
        assert!(a.flag(&["-i", "--interactive"]));
        assert_eq!(a.number::<u32>(&["--count"]), Ok(Some(2)));
        assert_eq!(a.value(&["--button"]), Ok(Some("secondary".into())));
        assert_eq!(a.positional().as_deref(), Some("@e3"));
        assert_eq!(a.positional().as_deref(), Some("--literal"));
        assert_eq!(a.finish(), Ok(()));
        let mut a = Args::new(["down", "-300"]);
        assert_eq!(a.rest(), ["down", "-300"]);
        assert_eq!(Args::new(["--nope"]).finish(), Err("unknown option --nope".into()));
        assert_eq!(Args::new(["x"]).finish(), Err("unexpected argument `x`".into()));
        assert_eq!(Args::new(["--timeout"]).value(&["--timeout"]), Err("--timeout needs a value".into()));
        assert!(Args::new(["--timeout", "soon"]).number::<u64>(&["--timeout"]).is_err());
    }
}
