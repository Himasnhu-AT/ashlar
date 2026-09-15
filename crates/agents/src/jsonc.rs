//! Edit one member of a JSON (or JSON-with-comments) object in place: everything else in the file
//! stays byte for byte as it was (other entries, their order, comments, indentation, trailing
//! commas). Agents' config files are the user's, and some (a coding agent's own state file) are
//! large and rewritten by the agent itself, so a parse-and-reserialize round trip is not an option.
//!
//! The scanner accepts JSON plus `//` and `/* */` comments and trailing commas, nests at most
//! [`MAX_DEPTH`] deep, and refuses anything else with an error (the file is then left alone).

use serde_json::Value;

/// Objects and arrays nested deeper than this are refused.
pub const MAX_DEPTH: usize = 256;

/// Why a file could not be edited.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("not valid JSON at byte {at}: {what}")]
pub struct SyntaxError {
    pub at: usize,
    pub what: String,
}

fn err<T>(at: usize, what: impl Into<String>) -> Result<T, SyntaxError> {
    Err(SyntaxError { at, what: what.into() })
}

/// One `"key": value` of an object, as byte offsets into the text.
#[derive(Debug, Clone)]
struct Member {
    key: String,
    /// The key's opening quote.
    start: usize,
    /// The value: `text[value.0..value.1]`.
    value: (usize, usize),
    /// The comma after the value, when there is one.
    comma: Option<usize>,
}

#[derive(Debug, Clone)]
struct Object {
    open: usize,
    close: usize,
    members: Vec<Member>,
}

struct Scanner<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Scanner<'a> {
    fn new(text: &'a str, at: usize) -> Self {
        Self { b: text.as_bytes(), i: at }
    }

    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    /// Skip whitespace and comments.
    fn skip(&mut self) -> Result<(), SyntaxError> {
        loop {
            match (self.peek(), self.b.get(self.i + 1).copied()) {
                (Some(b' ' | b'\t' | b'\n' | b'\r'), _) => self.i += 1,
                // a byte-order mark at the very start
                (Some(0xEF), _) if self.i == 0 && self.b.get(1..3) == Some(&[0xBB, 0xBF]) => self.i += 3,
                (Some(b'/'), Some(b'/')) => {
                    while self.peek().is_some_and(|c| c != b'\n') {
                        self.i += 1;
                    }
                }
                (Some(b'/'), Some(b'*')) => {
                    let from = self.i;
                    self.i += 2;
                    loop {
                        match (self.peek(), self.b.get(self.i + 1).copied()) {
                            (Some(b'*'), Some(b'/')) => {
                                self.i += 2;
                                break;
                            }
                            (Some(_), _) => self.i += 1,
                            (None, _) => return err(from, "unterminated comment"),
                        }
                    }
                }
                _ => return Ok(()),
            }
        }
    }

    fn string(&mut self) -> Result<String, SyntaxError> {
        let start = self.i;
        if self.peek() != Some(b'"') {
            return err(start, "expected a string");
        }
        self.i += 1;
        loop {
            match self.peek() {
                Some(b'"') => {
                    self.i += 1;
                    break;
                }
                Some(b'\\') => self.i += 2,
                Some(_) => self.i += 1,
                None => return err(start, "unterminated string"),
            }
        }
        let raw = self.b.get(start..self.i).and_then(|s| std::str::from_utf8(s).ok()).unwrap_or("\"\"");
        serde_json::from_str::<String>(raw).or_else(|e| err(start, format!("bad string ({e})")))
    }

    /// Skip one value; its end offset.
    fn value(&mut self, depth: usize) -> Result<usize, SyntaxError> {
        if depth > MAX_DEPTH {
            return err(self.i, "nested too deeply");
        }
        match self.peek() {
            Some(b'{') => Ok(self.object(depth)?.close + 1),
            Some(b'[') => {
                self.i += 1;
                loop {
                    self.skip()?;
                    match self.peek() {
                        Some(b']') => {
                            self.i += 1;
                            return Ok(self.i);
                        }
                        None => return err(self.i, "unterminated array"),
                        _ => {}
                    }
                    self.value(depth + 1)?;
                    self.skip()?;
                    match self.peek() {
                        Some(b',') => self.i += 1,
                        Some(b']') => {}
                        _ => return err(self.i, "expected `,` or `]`"),
                    }
                }
            }
            Some(b'"') => {
                self.string()?;
                Ok(self.i)
            }
            Some(c) if c == b'-' || c.is_ascii_alphanumeric() => {
                let start = self.i;
                while self.peek().is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'+' | b'.')) {
                    self.i += 1;
                }
                let word = self.b.get(start..self.i).and_then(|s| std::str::from_utf8(s).ok()).unwrap_or("");
                if serde_json::from_str::<Value>(word).is_err() {
                    return err(start, format!("unexpected `{word}`"));
                }
                Ok(self.i)
            }
            Some(_) => err(self.i, "expected a value"),
            None => err(self.i, "unexpected end"),
        }
    }

    /// Parse the object starting at the current `{`.
    fn object(&mut self, depth: usize) -> Result<Object, SyntaxError> {
        let open = self.i;
        if self.peek() != Some(b'{') {
            return err(open, "expected an object");
        }
        self.i += 1;
        let mut members = Vec::new();
        loop {
            self.skip()?;
            match self.peek() {
                Some(b'}') => {
                    let close = self.i;
                    self.i += 1;
                    return Ok(Object { open, close, members });
                }
                None => return err(open, "unterminated object"),
                _ => {}
            }
            let start = self.i;
            let key = self.string()?;
            self.skip()?;
            if self.peek() != Some(b':') {
                return err(self.i, "expected `:`");
            }
            self.i += 1;
            self.skip()?;
            let v0 = self.i;
            let v1 = self.value(depth + 1)?;
            self.skip()?;
            let comma = match self.peek() {
                Some(b',') => {
                    self.i += 1;
                    Some(self.i - 1)
                }
                Some(b'}') => None,
                _ => return err(self.i, "expected `,` or `}`"),
            };
            members.push(Member { key, start, value: (v0, v1), comma });
        }
    }
}

/// The document's top-level object (after checking nothing but comments follows it).
fn root(text: &str) -> Result<Object, SyntaxError> {
    let mut s = Scanner::new(text, 0);
    s.skip()?;
    let obj = s.object(0)?;
    s.skip()?;
    if s.i < text.len() {
        return err(s.i, "text after the top-level object");
    }
    Ok(obj)
}

fn object_at(text: &str, at: usize) -> Result<Object, SyntaxError> {
    Scanner::new(text, at).object(0)
}

/// The indentation of the line `at` is on.
fn line_indent(text: &str, at: usize) -> &str {
    let line_start = text.get(..at).and_then(|s| s.rfind('\n')).map_or(0, |n| n + 1);
    let line = text.get(line_start..).unwrap_or("");
    let n = line.len() - line.trim_start_matches([' ', '\t']).len();
    line.get(..n).unwrap_or("")
}

/// Indentation one step deeper than `indent`, in the file's style (tabs when it uses tabs).
fn deeper(indent: &str, unit: &str) -> String {
    format!("{indent}{unit}")
}

/// The file's indentation unit: the first member's indentation, else two spaces.
fn indent_unit(text: &str, root: &Object) -> String {
    match root.members.first() {
        Some(m) if text.get(..m.start).is_some_and(|s| s.trim_end_matches([' ', '\t']).ends_with('\n')) => {
            let i = line_indent(text, m.start);
            if i.is_empty() { "  ".into() } else { i.into() }
        }
        _ => "  ".into(),
    }
}

/// `value` pretty-printed, its continuation lines indented by `indent`.
fn render(value: &Value, indent: &str, unit: &str) -> String {
    let pretty = serde_json::to_string_pretty(value).unwrap_or_else(|_| "null".into());
    let mut out = String::with_capacity(pretty.len() + 64);
    for (n, line) in pretty.lines().enumerate() {
        if n > 0 {
            out.push('\n');
            out.push_str(indent);
        }
        // serde_json indents with two spaces per level
        let depth = (line.len() - line.trim_start_matches(' ').len()) / 2;
        for _ in 0..depth {
            out.push_str(unit);
        }
        out.push_str(line.trim_start_matches(' '));
    }
    out
}

fn key_text(key: &str) -> String {
    serde_json::to_string(key).unwrap_or_else(|_| "\"\"".into())
}

/// The value at `path` (object keys) in `text`, parsed; `None` when a key is missing or a step is
/// not an object. An empty or blank text has no values.
pub fn get(text: &str, path: &[&str]) -> Result<Option<Value>, SyntaxError> {
    if text.trim().is_empty() {
        return Ok(None);
    }
    let mut obj = root(text)?;
    let Some((last, parents)) = path.split_last() else { return Ok(None) };
    for k in parents {
        let Some(m) = obj.members.iter().rev().find(|m| m.key == *k) else { return Ok(None) };
        if text.as_bytes().get(m.value.0) != Some(&b'{') {
            return Ok(None);
        }
        obj = object_at(text, m.value.0)?;
    }
    let Some(m) = obj.members.iter().rev().find(|m| m.key == *last) else { return Ok(None) };
    let raw = text.get(m.value.0..m.value.1).unwrap_or("null");
    Ok(Some(serde_json::from_str(&strip_comments(raw)).unwrap_or(Value::Null)))
}

/// `raw` (one JSON-with-comments value) as plain JSON: comments blanked, trailing commas dropped.
fn strip_comments(raw: &str) -> String {
    let b = raw.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while let Some(&c) = b.get(i) {
        match (c, b.get(i + 1).copied()) {
            (b'"', _) => {
                let start = i;
                i += 1;
                while let Some(&d) = b.get(i) {
                    i += if d == b'\\' { 2 } else { 1 };
                    if d == b'"' {
                        break;
                    }
                }
                out.extend_from_slice(b.get(start..i.min(b.len())).unwrap_or(b""));
            }
            (b'/', Some(b'/')) => {
                while b.get(i).is_some_and(|d| *d != b'\n') {
                    i += 1;
                }
            }
            (b'/', Some(b'*')) => {
                i += 2;
                while b.get(i).is_some() && !(b.get(i) == Some(&b'*') && b.get(i + 1) == Some(&b'/')) {
                    i += 1;
                }
                i += 2;
            }
            (b',', _) => {
                let next = b.get(i + 1..).and_then(|r| r.iter().find(|x| !x.is_ascii_whitespace()));
                if !matches!(next, Some(b'}' | b']')) {
                    out.push(b',');
                }
                i += 1;
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8(out).unwrap_or_default()
}

/// `text` with `path` (object keys, the last one the member) set to `value`: the member's value is
/// replaced when it exists, else the member is added at the end of its object; missing parent
/// objects are created. A blank `text` becomes a new document. A parent that exists but is not an
/// object is an error.
pub fn set(text: &str, path: &[&str], value: &Value) -> Result<String, SyntaxError> {
    let Some((last, parents)) = path.split_last() else { return err(0, "empty path") };
    if text.trim().is_empty() {
        let mut v = value.clone();
        for k in path.iter().rev() {
            let mut m = serde_json::Map::new();
            m.insert((*k).to_string(), v);
            v = Value::Object(m);
        }
        return Ok(format!("{}\n", render(&v, "", "  ")));
    }
    let top = root(text)?;
    let unit = indent_unit(text, &top);
    let mut obj = top;
    for (n, k) in parents.iter().enumerate() {
        match obj.members.iter().rev().find(|m| m.key == *k) {
            Some(m) if text.as_bytes().get(m.value.0) == Some(&b'{') => obj = object_at(text, m.value.0)?,
            Some(m) => return err(m.value.0, format!("`{}` is not an object", path.get(..=n).unwrap_or_default().join("."))),
            None => {
                // build the rest of the path as one new member
                let mut v = value.clone();
                for k in path.iter().skip(n + 1).rev() {
                    let mut m = serde_json::Map::new();
                    m.insert((*k).to_string(), v);
                    v = Value::Object(m);
                }
                return Ok(insert(text, &obj, k, &v, &unit));
            }
        }
    }
    match obj.members.iter().rev().find(|m| m.key == *last) {
        Some(m) => {
            let indent = line_indent(text, m.start).to_string();
            let mut out = String::with_capacity(text.len() + 64);
            out.push_str(text.get(..m.value.0).unwrap_or(""));
            out.push_str(&render(value, &indent, &unit));
            out.push_str(text.get(m.value.1..).unwrap_or(""));
            Ok(out)
        }
        None => Ok(insert(text, &obj, last, value, &unit)),
    }
}

/// Add `"key": value` at the end of `obj`.
fn insert(text: &str, obj: &Object, key: &str, value: &Value, unit: &str) -> String {
    let parent_indent = line_indent(text, obj.open).to_string();
    let indent = match obj.members.first() {
        Some(m) if text.get(..m.start).is_some_and(|s| s.trim_end_matches([' ', '\t']).ends_with('\n')) => line_indent(text, m.start).to_string(),
        _ => deeper(&parent_indent, unit),
    };
    let member = format!("{}: {}", key_text(key), render(value, &indent, unit));
    let mut out = String::with_capacity(text.len() + member.len() + 16);
    match obj.members.last() {
        Some(last) => match last.comma {
            // a trailing comma stays a trailing comma
            Some(c) => {
                out.push_str(text.get(..=c).unwrap_or(""));
                out.push_str(&format!("\n{indent}{member},"));
                out.push_str(text.get(c + 1..).unwrap_or(""));
            }
            None => {
                out.push_str(text.get(..last.value.1).unwrap_or(""));
                out.push_str(&format!(",\n{indent}{member}"));
                out.push_str(text.get(last.value.1..).unwrap_or(""));
            }
        },
        None => {
            let inner = text.get(obj.open + 1..obj.close).unwrap_or("");
            out.push_str(text.get(..=obj.open).unwrap_or(""));
            if inner.trim().is_empty() {
                out.push_str(&format!("\n{indent}{member}\n{parent_indent}"));
            } else {
                // keep the comments that were inside
                out.push_str(&format!("\n{indent}{member}"));
                out.push_str(inner);
            }
            out.push_str(text.get(obj.close..).unwrap_or(""));
        }
    }
    out
}

/// `text` without the member at `path`; `None` when there is no such member (nothing to do).
pub fn remove(text: &str, path: &[&str]) -> Result<Option<String>, SyntaxError> {
    let Some((last, parents)) = path.split_last() else { return Ok(None) };
    if text.trim().is_empty() {
        return Ok(None);
    }
    let mut obj = root(text)?;
    for k in parents {
        match obj.members.iter().rev().find(|m| m.key == *k) {
            Some(m) if text.as_bytes().get(m.value.0) == Some(&b'{') => obj = object_at(text, m.value.0)?,
            _ => return Ok(None),
        }
    }
    let Some(idx) = obj.members.iter().rposition(|m| m.key == *last) else { return Ok(None) };
    let Some(m) = obj.members.get(idx) else { return Ok(None) };
    let prev = idx.checked_sub(1).and_then(|p| obj.members.get(p));
    let (from, to) = match (m.comma, prev) {
        // from the end of what came before through this member's comma
        (Some(c), _) => (prev.and_then(|p| p.comma).map_or(obj.open + 1, |pc| pc + 1), c + 1),
        // the last member: also drop the comma before it
        (None, Some(p)) => (p.comma.unwrap_or(p.value.1), m.value.1),
        (None, None) => (obj.open + 1, m.value.1),
    };
    let mut out = String::with_capacity(text.len());
    out.push_str(text.get(..from).unwrap_or(""));
    out.push_str(text.get(to..).unwrap_or(""));
    // an object left with only whitespace closes up: `{}`
    if obj.members.len() == 1 {
        let close = obj.close - (to - from);
        if out.get(obj.open + 1..close).is_some_and(|s| s.trim().is_empty()) {
            out.replace_range(obj.open + 1..close, "");
        }
    }
    Ok(Some(out))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const CFG: &str = r#"{
  // the user's own servers
  "theme": "dark",
  "mcpServers": {
    "other": { "command": "/opt/other", "args": ["serve"] },
  },
  "tail": [1, 2, {"x": null}]
}
"#;

    #[test]
    fn adds_a_member_and_keeps_everything_else() {
        let out = set(CFG, &["mcpServers", "app"], &json!({"command": "/bin/app", "args": ["mcp"]})).unwrap();
        assert!(out.starts_with("{\n  // the user's own servers\n  \"theme\": \"dark\",\n  \"mcpServers\": {\n    \"other\": { \"command\": \"/opt/other\", \"args\": [\"serve\"] },\n    \"app\": {\n      \"args\": [\n        \"mcp\"\n      ],\n      \"command\": \"/bin/app\"\n    },\n  },"), "{out}");
        assert!(out.ends_with("  \"tail\": [1, 2, {\"x\": null}]\n}\n"));
        assert_eq!(get(&out, &["mcpServers", "app", "command"]).unwrap(), Some(json!("/bin/app")));
        assert_eq!(get(&out, &["mcpServers", "other", "args"]).unwrap(), Some(json!(["serve"])));
    }

    #[test]
    fn set_is_idempotent_and_replaces_in_place() {
        let v = json!({"command": "/bin/app"});
        let once = set(CFG, &["mcpServers", "app"], &v).unwrap();
        assert_eq!(set(&once, &["mcpServers", "app"], &v).unwrap(), once);
        let moved = set(&once, &["mcpServers", "app"], &json!({"command": "/new/app"})).unwrap();
        assert_eq!(moved.matches("\"app\"").count(), 1);
        assert_eq!(get(&moved, &["mcpServers", "app"]).unwrap(), Some(json!({"command": "/new/app"})));
    }

    #[test]
    fn creates_missing_parents_and_new_files() {
        let out = set("{\n\t\"a\": 1\n}", &["servers", "app"], &json!({"type": "stdio"})).unwrap();
        assert_eq!(out, "{\n\t\"a\": 1,\n\t\"servers\": {\n\t\t\"app\": {\n\t\t\t\"type\": \"stdio\"\n\t\t}\n\t}\n}");
        assert_eq!(set("", &["servers", "app"], &json!(1)).unwrap(), "{\n  \"servers\": {\n    \"app\": 1\n  }\n}\n");
        assert_eq!(set("{}", &["k"], &json!(true)).unwrap(), "{\n  \"k\": true\n}");
        assert_eq!(set("{ /* keep */ }", &["k"], &json!(true)).unwrap(), "{\n  \"k\": true /* keep */ }");
    }

    #[test]
    fn remove_restores_the_original_bytes() {
        let added = set(CFG, &["mcpServers", "app"], &json!({"command": "/bin/app"})).unwrap();
        assert_eq!(remove(&added, &["mcpServers", "app"]).unwrap().unwrap(), CFG);
        let plain = "{\n  \"mcpServers\": {\n    \"other\": 1\n  }\n}\n";
        let added = set(plain, &["mcpServers", "app"], &json!(2)).unwrap();
        assert_eq!(remove(&added, &["mcpServers", "app"]).unwrap().unwrap(), plain);
        // first of two, and the only one
        assert_eq!(remove("{\"a\": 1, \"b\": 2}", &["a"]).unwrap().unwrap(), "{ \"b\": 2}");
        assert_eq!(remove("{\n  \"a\": {\"x\": 1}\n}", &["a", "x"]).unwrap().unwrap(), "{\n  \"a\": {}\n}");
        assert_eq!(remove(CFG, &["mcpServers", "app"]).unwrap(), None);
        assert_eq!(remove(CFG, &["nothere", "app"]).unwrap(), None);
    }

    #[test]
    fn refuses_what_it_cannot_read_and_wrong_shapes() {
        for bad in ["{", "[1]", "{\"a\": }", "{\"a\": 1} trailing", "{\"a\" 1}", "{\"a\": tru}", "/* open"] {
            assert!(set(bad, &["x"], &json!(1)).is_err(), "{bad}");
        }
        assert!(set("{\"mcpServers\": []}", &["mcpServers", "x"], &json!(1)).unwrap_err().what.contains("not an object"));
        let deep = format!("{{\"a\": {}{}}}", "[".repeat(MAX_DEPTH + 2), "]".repeat(MAX_DEPTH + 2));
        assert!(set(&deep, &["x"], &json!(1)).is_err());
    }

    #[test]
    fn strings_with_braces_and_escapes_are_opaque() {
        let t = r#"{"a": "}{\"//", "b": "/* not a comment */"}"#;
        let out = set(t, &["c"], &json!("x")).unwrap();
        assert_eq!(get(&out, &["a"]).unwrap(), Some(json!("}{\"//")));
        assert_eq!(get(&out, &["b"]).unwrap(), Some(json!("/* not a comment */")));
        assert_eq!(get(&out, &["c"]).unwrap(), Some(json!("x")));
    }

    #[test]
    fn get_reads_values_with_comments_inside() {
        let t = "{\"s\": {\n  // c\n  \"x\": [1, /* two */ 2,],\n}}";
        assert_eq!(get(t, &["s"]).unwrap(), Some(json!({"x": [1, 2]})));
    }
}
