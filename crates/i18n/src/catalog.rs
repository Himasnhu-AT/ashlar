//! Catalogs: flat JSON objects of `key → text`, parsed on first use; bad input is reported, never
//! a panic.

use std::borrow::Cow;
use std::collections::HashMap;

/// Largest catalog text accepted (bytes).
pub const MAX_CATALOG_BYTES: usize = 8 << 20;
/// Deepest nesting flattened (`{"a": {"b": "x"}}` is the key `a.b`).
const MAX_DEPTH: usize = 8;

/// Why a catalog was rejected (it then acts as empty, so lookups fall back).
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CatalogError {
    #[error("catalog for {locale} is not valid JSON: {message}")]
    Json { locale: String, message: String },
    #[error("catalog for {locale} is not a JSON object")]
    NotAnObject { locale: String },
    #[error("catalog for {locale} is larger than {MAX_CATALOG_BYTES} bytes")]
    TooLarge { locale: String },
    #[error("catalog for {locale}: {key} is not a string")]
    NotAString { locale: String, key: String },
}

/// Parsed `key → text` entries.
#[derive(Clone, Debug, Default)]
pub(crate) struct Entries {
    pub map: HashMap<Box<str>, Box<str>>,
}

/// Parses a catalog. Non-string values are skipped and reported in `problems`; nested objects
/// are flattened with dots.
pub(crate) fn parse(locale: &str, json: &str, problems: &mut Vec<CatalogError>) -> Entries {
    let mut entries = Entries::default();
    if json.len() > MAX_CATALOG_BYTES {
        problems.push(CatalogError::TooLarge { locale: locale.to_owned() });
        return entries;
    }
    let value: serde_json::Value = match serde_json::from_str(json) {
        Ok(v) => v,
        Err(e) => {
            problems.push(CatalogError::Json { locale: locale.to_owned(), message: e.to_string() });
            return entries;
        }
    };
    let serde_json::Value::Object(root) = value else {
        problems.push(CatalogError::NotAnObject { locale: locale.to_owned() });
        return entries;
    };
    flatten(locale, "", &root, 0, &mut entries, problems);
    entries
}

fn flatten(
    locale: &str,
    prefix: &str,
    obj: &serde_json::Map<String, serde_json::Value>,
    depth: usize,
    out: &mut Entries,
    problems: &mut Vec<CatalogError>,
) {
    for (k, v) in obj {
        let key: Cow<'_, str> = if prefix.is_empty() { Cow::Borrowed(k) } else { Cow::Owned(format!("{prefix}.{k}")) };
        match v {
            serde_json::Value::String(s) => {
                out.map.insert(key.as_ref().into(), s.as_str().into());
            }
            serde_json::Value::Object(inner) if depth < MAX_DEPTH => flatten(locale, &key, inner, depth + 1, out, problems),
            _ => problems.push(CatalogError::NotAString { locale: locale.to_owned(), key: key.into_owned() }),
        }
    }
}

/// Fills `{name}` placeholders from `args`; `{{` and `}}` are literal braces; unknown names stay
/// as written.
pub fn fill(template: &str, args: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut rest = template;
    while let Some(at) = rest.find(['{', '}']) {
        out.push_str(rest.get(..at).unwrap_or(""));
        let tail = rest.get(at..).unwrap_or("");
        if tail.starts_with("{{") {
            out.push('{');
            rest = tail.get(2..).unwrap_or("");
        } else if tail.starts_with("}}") {
            out.push('}');
            rest = tail.get(2..).unwrap_or("");
        } else if tail.starts_with('{')
            && let Some(end) = tail.find('}')
        {
            let name = tail.get(1..end).unwrap_or("");
            match args.iter().find(|(n, _)| *n == name) {
                Some((_, value)) => out.push_str(value),
                None => out.push_str(tail.get(..=end).unwrap_or("")),
            }
            rest = tail.get(end + 1..).unwrap_or("");
        } else {
            // a lone brace
            out.push_str(tail.get(..1).unwrap_or(""));
            rest = tail.get(1..).unwrap_or("");
        }
    }
    out.push_str(rest);
    out
}

/// The `{name}` placeholders in a template, sorted and deduplicated.
pub fn placeholders(template: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = template;
    while let Some(at) = rest.find('{') {
        let tail = rest.get(at..).unwrap_or("");
        if tail.starts_with("{{") {
            rest = tail.get(2..).unwrap_or("");
            continue;
        }
        match tail.find('}') {
            Some(end) => {
                if let Some(name) = tail.get(1..end) {
                    out.push(name);
                }
                rest = tail.get(end + 1..).unwrap_or("");
            }
            None => break,
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates() {
        assert_eq!(fill("Hello, {name}!", &[("name", "Ada")]), "Hello, Ada!");
        assert_eq!(fill("{a}{b}{a}", &[("a", "1"), ("b", "2")]), "121");
        assert_eq!(fill("{missing} {{x}} }}", &[]), "{missing} {x} }");
        assert_eq!(fill("lone { and } braces", &[]), "lone { and } braces");
        assert_eq!(fill("{ünï}", &[("ünï", "ok")]), "ok");
        assert_eq!(placeholders("{b} {a} {{c}} {b}"), ["a", "b"]);
    }

    #[test]
    fn bad_catalogs_are_reported() {
        let mut problems = Vec::new();
        assert!(parse("de", "{not json", &mut problems).map.is_empty());
        assert!(parse("de", "[1, 2]", &mut problems).map.is_empty());
        let e = parse("de", r#"{"a": "x", "b": 3, "c": {"d": "y"}}"#, &mut problems);
        assert_eq!(e.map.get("a").map(|s| &**s), Some("x"));
        assert_eq!(e.map.get("c.d").map(|s| &**s), Some("y"));
        assert_eq!(problems.len(), 3);
        let deep = format!("{}\"x\"{}", "{\"k\":".repeat(200), "}".repeat(200));
        let _ = parse("de", &deep, &mut problems);
    }
}
