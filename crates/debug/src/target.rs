//! Element targets as written on the command line, and as the control protocol takes them.
//!
//! | Written | Sent |
//! |---|---|
//! | `@e3`, `e3` | `{"ref": "e3"}` (from the latest snapshot) |
//! | `id=file.save`, `#file.save` | `{"id": "file.save"}` (automation id) |
//! | `text=Save`, `button=Save`, `role:button=Save` | `{"text": "Save"}`, `{"text": "Save", "role": "button"}` |
//! | `120,40` | `{"x": 120, "y": 40}` (logical points) |

use serde_json::{Value, json};

/// Whether `s` is a ref: `e` followed by digits.
fn is_ref(s: &str) -> bool {
    s.strip_prefix('e').is_some_and(|d| !d.is_empty() && d.chars().all(|c| c.is_ascii_digit()))
}

/// Roles that can prefix a text target (`button=Save`).
const ROLES: &[&str] =
    &["button", "link", "textbox", "checkbox", "radio", "label", "menuitem", "tab", "slider", "combobox", "listitem", "heading", "image", "window"];

/// Parse one target. The error says which forms exist.
pub fn parse(s: &str) -> Result<Value, String> {
    let s = s.trim();
    if let Some(r) = s.strip_prefix('@') {
        return if r.is_empty() { Err("`@` needs a ref, like @e3".into()) } else { Ok(json!({"ref": r})) };
    }
    if is_ref(s) {
        return Ok(json!({"ref": s}));
    }
    if let Some(id) = s.strip_prefix("id=").or_else(|| s.strip_prefix('#')).filter(|i| !i.is_empty()) {
        return Ok(json!({"id": id}));
    }
    if let Some(t) = s.strip_prefix("text=") {
        return Ok(json!({"text": t}));
    }
    if let Some((role, text)) = s.split_once('=') {
        let role = role.strip_prefix("role:").unwrap_or(role);
        if ROLES.contains(&role) || s.starts_with("role:") {
            return Ok(json!({"text": text, "role": role}));
        }
    }
    if let Some((x, y)) = s.split_once(',')
        && let (Ok(x), Ok(y)) = (x.trim().parse::<f64>(), y.trim().parse::<f64>())
        && x.is_finite()
        && y.is_finite()
    {
        return Ok(json!({"x": x, "y": y}));
    }
    Err(format!("`{s}` is not a target; write a ref (@e3), id=<automation id>, text=<label>, <role>=<label> or x,y"))
}

/// A target for display (`@e3`, `id=file.save`, …).
pub fn show(t: &Value) -> String {
    let s = |k: &str| t.get(k).and_then(Value::as_str);
    if let Some(r) = s("ref") {
        return format!("@{r}");
    }
    if let Some(i) = s("id") {
        return format!("id={i}");
    }
    if let Some(x) = s("text") {
        return match s("role") {
            Some(r) => format!("{r}={x}"),
            None => format!("text={x}"),
        };
    }
    match (t.get("x").and_then(Value::as_f64), t.get("y").and_then(Value::as_f64)) {
        (Some(x), Some(y)) => format!("{x},{y}"),
        _ => t.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_form() {
        assert_eq!(parse("@e3").unwrap(), json!({"ref": "e3"}));
        assert_eq!(parse("e12").unwrap(), json!({"ref": "e12"}));
        assert_eq!(parse("@custom").unwrap(), json!({"ref": "custom"}));
        assert_eq!(parse("id=file.save").unwrap(), json!({"id": "file.save"}));
        assert_eq!(parse("#file.save").unwrap(), json!({"id": "file.save"}));
        assert_eq!(parse("text=Save as…").unwrap(), json!({"text": "Save as…"}));
        assert_eq!(parse("button=Save").unwrap(), json!({"text": "Save", "role": "button"}));
        assert_eq!(parse("role:toolbar=Main").unwrap(), json!({"text": "Main", "role": "toolbar"}));
        assert_eq!(parse("120,40.5").unwrap(), json!({"x": 120.0, "y": 40.5}));
        for bad in ["", "@", "e", "Save", "nan,1", "inf,2", "a=b", "#"] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn show_inverts_parse() {
        for s in ["@e3", "id=file.save", "text=Save", "button=Save", "1,2"] {
            assert_eq!(show(&parse(s).unwrap()), s);
        }
    }
}
