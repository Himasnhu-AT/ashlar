//! Parsing of `application/x-www-form-urlencoded` data, used for query strings and form bodies.

use std::time::Duration;

use http_body_util::{BodyExt, Limited};

pub use super::data::Params;

/// Largest form body accepted, in bytes.
const MAX_BODY: usize = 16 * 1024;
const BODY_TIMEOUT: Duration = Duration::from_secs(10);

/// Most fields kept from one query or form. Extra ones are ignored.
const MAX_FIELDS: usize = 32;
/// Longest value kept, in bytes.
const MAX_VALUE: usize = 2048;

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Decodes `%XX` escapes and `+`. Invalid escapes are kept literally; invalid UTF-8 is replaced.
pub fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() && hex(bytes[i + 1]).is_some() && hex(bytes[i + 2]).is_some() => {
                out.push(hex(bytes[i + 1]).unwrap_or(0) * 16 + hex(bytes[i + 2]).unwrap_or(0));
                i += 3;
            }
            other => {
                out.push(other);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Parses `a=1&b=2`. The first occurrence of a name wins.
pub fn parse(text: &str) -> Params {
    let mut out = Params::new();
    for pair in text.split('&').filter(|pair| !pair.is_empty()).take(MAX_FIELDS) {
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        let mut value = decode(value);
        if value.len() > MAX_VALUE {
            let mut cut = MAX_VALUE;
            while !value.is_char_boundary(cut) {
                cut -= 1;
            }
            value.truncate(cut);
        }
        out.entry(decode(name)).or_insert(value);
    }
    out
}

/// Reads and parses a form body. `None` when it is too large, times out or cannot be read.
pub async fn read_body(body: axum::body::Body) -> Option<Params> {
    let collected = tokio::time::timeout(BODY_TIMEOUT, Limited::new(body, MAX_BODY).collect()).await.ok()?.ok()?;
    Some(parse(&String::from_utf8_lossy(&collected.to_bytes())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_escapes_and_plus() {
        assert_eq!(decode("a%20b+c%2Fd"), "a b c/d");
        assert_eq!(decode("100%"), "100%");
        assert_eq!(decode("%zz%4"), "%zz%4");
        assert_eq!(decode("%C3%A9"), "\u{e9}");
        assert_eq!(decode("%FF"), "\u{fffd}");
    }

    #[test]
    fn parses_pairs_and_keeps_the_first_duplicate() {
        let p = parse("days=7&version=1.0%2B2&flag&days=30&=x");
        assert_eq!(p.get("days").map(String::as_str), Some("7"));
        assert_eq!(p.get("version").map(String::as_str), Some("1.0+2"));
        assert_eq!(p.get("flag").map(String::as_str), Some(""));
        assert!(parse("").is_empty());
    }

    #[test]
    fn bounds_fields_and_values() {
        let many: String = (0..100).map(|i| format!("k{i}=v&")).collect();
        assert_eq!(parse(&many).len(), MAX_FIELDS);
        let long = format!("a={}", "\u{e9}".repeat(5000));
        assert!(parse(&long)["a"].len() <= MAX_VALUE);
    }
}
