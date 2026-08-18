//! Legacy free-text params docs (`{"name":str,"width":u32=1920,"mix":"Stereo|Mono"?}`,
//! `{layers: [id|name|#n], add?}`, `{time? (s) | frame?}`): enough parsing to refuse keys a doc
//! clearly does not mention and to give MCP a usable schema, so existing command tables port
//! without typing every parameter first.

use serde_json::{Map, Value, json};

use crate::params::{json_type, unknown_keys_error};

/// Docs longer than this are not parsed (they are `'static` text, but stay bounded).
const MAX_DOC: usize = 8192;

/// Scan state shared by the splitters: bracket depth and whether we are inside a `"string"`.
#[derive(Default)]
struct Scan {
    depth: i32,
    in_str: bool,
    escaped: bool,
}

impl Scan {
    /// Feed one char; returns whether it is structural (outside any string).
    fn feed(&mut self, c: char) -> bool {
        if self.in_str {
            if self.escaped {
                self.escaped = false;
            } else if c == '\\' {
                self.escaped = true;
            } else if c == '"' {
                self.in_str = false;
            }
            return false;
        }
        if c == '"' {
            self.in_str = true;
            return false;
        }
        true
    }
}

/// The top-level pieces of a `{…}` doc, split at depth-0 commas; `None` when the doc is not a
/// `{…}` key list.
fn pieces(doc: &str) -> Option<Vec<&str>> {
    if doc.len() > MAX_DOC {
        return None;
    }
    let body = doc.trim().strip_prefix('{')?;
    let mut out = Vec::new();
    let mut scan = Scan::default();
    let mut start = 0;
    fn push<'a>(s: Option<&'a str>, out: &mut Vec<&'a str>) {
        if let Some(s) = s.map(str::trim).filter(|s| !s.is_empty()) {
            out.push(s);
        }
    }
    for (i, c) in body.char_indices() {
        if !scan.feed(c) {
            continue;
        }
        match c {
            '{' | '[' | '(' => scan.depth += 1,
            ']' | ')' => scan.depth -= 1,
            '}' if scan.depth == 0 => {
                push(body.get(start..i), &mut out);
                return Some(out);
            }
            '}' => scan.depth -= 1,
            ',' if scan.depth == 0 => {
                push(body.get(start..i), &mut out);
                start = i + c.len_utf8();
            }
            _ => {}
        }
    }
    None
}

/// Split a piece into its key part and value part at the first top-level `:`.
fn key_and_value(piece: &str) -> (&str, Option<&str>) {
    let mut scan = Scan::default();
    for (i, c) in piece.char_indices() {
        if !scan.feed(c) {
            continue;
        }
        match c {
            '{' | '[' | '(' => scan.depth += 1,
            '}' | ']' | ')' => scan.depth -= 1,
            ':' if scan.depth == 0 => return (piece.get(..i).unwrap_or_default(), piece.get(i + 1..)),
            _ => {}
        }
    }
    (piece, None)
}

/// The keys named by a piece's key part: `"name"`, `layers`, `path|prop`, `time? (s) | frame?`.
fn piece_keys(key_part: &str) -> Vec<String> {
    let mut keys = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    let take = |s: &str, keys: &mut Vec<String>| {
        let k: String = s.trim().trim_start_matches('"').chars().take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '-').collect();
        if !k.is_empty() && !keys.contains(&k) {
            keys.push(k);
        }
    };
    for (i, c) in key_part.char_indices() {
        match c {
            '{' | '[' | '(' => depth += 1,
            '}' | ']' | ')' => depth -= 1,
            '|' if depth == 0 => {
                take(key_part.get(start..i).unwrap_or_default(), &mut keys);
                start = i + 1;
            }
            _ => {}
        }
    }
    take(key_part.get(start..).unwrap_or_default(), &mut keys);
    keys
}

/// Top-level keys accepted by a params doc; `None` when the doc isn't a `{…}` key list (then
/// nothing is refused).
pub fn accepted_params(doc: &str) -> Option<Vec<String>> {
    let mut keys: Vec<String> = Vec::new();
    for piece in pieces(doc)? {
        for k in piece_keys(key_and_value(piece).0) {
            if !keys.contains(&k) {
                keys.push(k);
            }
        }
    }
    Some(keys)
}

/// The piece's text with `"strings"` removed (for spotting `?` and `=` markers).
fn outside_strings(piece: &str) -> String {
    let mut scan = Scan::default();
    piece.chars().filter(|c| scan.feed(*c) && *c != '"').collect()
}

/// A JSON Schema type guessed from a value doc (`str`, `u32=1920`, `"A|B"?`, `[id]`), only when
/// the doc says so plainly.
fn value_schema(value: &str) -> Map<String, Value> {
    let mut m = Map::new();
    let v = value.trim().trim_end_matches('?').trim();
    let v = v.split('=').next().unwrap_or_default().trim();
    if v.starts_with('[') {
        m.insert("type".into(), "array".into());
    } else if v.starts_with('{') {
        m.insert("type".into(), "object".into());
    } else if let Some(inner) = v.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
        m.insert("type".into(), "string".into());
        if inner.contains('|') && !inner.contains('…') && !inner.contains("...") {
            let values: Vec<&str> = inner.split('|').map(str::trim).filter(|s| !s.is_empty()).collect();
            m.insert("enum".into(), values.into());
        }
    } else {
        let t = match v.to_ascii_lowercase().as_str() {
            "str" | "string" | "text" | "name" | "path" => Some("string"),
            "bool" | "boolean" => Some("boolean"),
            "u8" | "u16" | "u32" | "u64" | "usize" | "i8" | "i16" | "i32" | "i64" | "isize" | "int" | "integer" | "n" => Some("integer"),
            "f32" | "f64" | "float" | "number" | "num" => Some("number"),
            _ => None,
        };
        if let Some(t) = t {
            m.insert("type".into(), t.into());
        }
    }
    m
}

/// The JSON Schema of a legacy params doc: one property per accepted key (typed when the doc says
/// so plainly, described by its piece of the doc), no others.
pub(crate) fn schema(doc: &str) -> Value {
    let Some(pieces) = pieces(doc) else {
        return json!({"type": "object", "description": doc});
    };
    let mut props = Map::new();
    let mut required: Vec<String> = Vec::new();
    for piece in pieces {
        let (key_part, value) = key_and_value(piece);
        let keys = piece_keys(key_part);
        let markers = outside_strings(piece);
        // `key?`, `key: t?`, `key: t=default` are optional; so is any key with alternatives
        let optional = keys.len() > 1 || markers.contains('?') || markers.contains('=');
        for k in keys {
            if !optional && !required.contains(&k) {
                required.push(k.clone());
            }
            let mut p = value.map(value_schema).unwrap_or_default();
            p.insert("description".into(), piece.into());
            props.insert(k, Value::Object(p));
        }
    }
    json!({"type": "object", "description": doc, "properties": props, "required": required, "additionalProperties": false})
}

/// Loose validation: refuse keys the doc clearly does not mention.
pub(crate) fn check(doc: &str, value: &Value, common: &[&str], aliases: &[(&str, &str)]) -> Result<(), String> {
    let Some(accepted) = accepted_params(doc) else { return Ok(()) };
    let obj = match value {
        Value::Null => return Ok(()),
        Value::Object(m) => m,
        other => return Err(format!("expected an object of parameters, got {}", json_type(other))),
    };
    let ok = |k: &str| common.contains(&k) || accepted.iter().any(|a| a == k || aliases.iter().any(|(doc, alias)| doc == a && *alias == k));
    let unknown: Vec<&str> = obj.keys().map(String::as_str).filter(|k| !ok(k)).collect();
    if unknown.is_empty() {
        return Ok(());
    }
    let accepted: Vec<&str> = accepted.iter().map(String::as_str).collect();
    Err(format!("{} (doc: {doc})", unknown_keys_error(&unknown, &accepted)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_of_both_doc_styles() {
        let film = r#"{"name":str,"width":u32=1920,"fps":f64=23.976,"mix":"Stereo|Mono|5.1|Adaptive"?,"fromItem":itemId?}"#;
        assert_eq!(accepted_params(film).unwrap(), ["name", "width", "fps", "mix", "fromItem"]);
        assert_eq!(accepted_params("{layers: [id|name|#n], add?, toggle?}").unwrap(), ["layers", "add", "toggle"]);
        assert_eq!(accepted_params("{time? (s) | frame? | timecode?}").unwrap(), ["time", "frame", "timecode"]);
        assert_eq!(accepted_params("{}").unwrap(), Vec::<String>::new());
        assert_eq!(accepted_params(r#"{"label":"Violet|Iris|…"}"#).unwrap(), ["label"]);
        // a comma or brace inside a quoted value does not split
        assert_eq!(accepted_params(r#"{"sep":", or }","x":n}"#).unwrap(), ["sep", "x"]);
        assert_eq!(accepted_params("free text, no braces"), None);
        assert_eq!(accepted_params("{unterminated"), None);
    }

    #[test]
    fn schema_types_and_required() {
        let s = schema(r#"{"name":str,"width":u32=1920,"mix":"Stereo|Mono"?,"clips":[id]?}"#);
        assert_eq!(s["required"], json!(["name"]));
        assert_eq!(s["properties"]["name"]["type"], "string");
        assert_eq!(s["properties"]["width"]["type"], "integer");
        assert_eq!(s["properties"]["mix"]["enum"], json!(["Stereo", "Mono"]));
        assert_eq!(s["properties"]["clips"]["type"], "array");
        assert_eq!(s["additionalProperties"], false);
    }
}
