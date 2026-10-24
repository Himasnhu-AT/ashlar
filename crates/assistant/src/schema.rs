//! A small JSON Schema check for tool arguments before a tool runs: `type` (one or a list),
//! `required`, `properties`, `additionalProperties: false`, `items`, `enum`, `const`, `anyOf` /
//! `oneOf` (any branch). Keywords it does not know pass. Depth is bounded.

use serde_json::Value;

const MAX_DEPTH: usize = 32;

/// `Err` names the first mismatch by its path (`params.clips[2]`).
pub fn validate(schema: &Value, value: &Value) -> Result<(), String> {
    check(schema, value, "arguments", 0)
}

fn type_ok(t: &str, v: &Value) -> bool {
    match t {
        "object" => v.is_object(),
        "array" => v.is_array(),
        "string" => v.is_string(),
        "boolean" => v.is_boolean(),
        "null" => v.is_null(),
        "number" => v.is_number(),
        "integer" => v.as_i64().is_some() || v.as_u64().is_some() || v.as_f64().is_some_and(|f| f.fract() == 0.0),
        _ => true,
    }
}

fn kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

fn check(schema: &Value, v: &Value, path: &str, depth: usize) -> Result<(), String> {
    if depth > MAX_DEPTH {
        return Ok(());
    }
    let Some(s) = schema.as_object() else { return Ok(()) };
    match s.get("type") {
        Some(Value::String(t)) if !type_ok(t, v) => return Err(format!("`{path}` must be {t}, got {}", kind(v))),
        Some(Value::Array(ts)) if !ts.iter().filter_map(Value::as_str).any(|t| type_ok(t, v)) => {
            let names: Vec<&str> = ts.iter().filter_map(Value::as_str).collect();
            return Err(format!("`{path}` must be {}, got {}", names.join(" or "), kind(v)));
        }
        _ => {}
    }
    if let Some(e) = s.get("enum").and_then(Value::as_array)
        && !e.contains(v)
    {
        return Err(format!("`{path}` must be one of {}", Value::Array(e.clone())));
    }
    if let Some(c) = s.get("const")
        && c != v
    {
        return Err(format!("`{path}` must be {c}"));
    }
    for key in ["anyOf", "oneOf"] {
        if let Some(branches) = s.get(key).and_then(Value::as_array)
            && !branches.is_empty()
            && !branches.iter().any(|b| check(b, v, path, depth + 1).is_ok())
        {
            return Err(format!("`{path}` matches none of the allowed shapes"));
        }
    }
    if let Some(o) = v.as_object() {
        let props = s.get("properties").and_then(Value::as_object);
        for r in s.get("required").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
            if !o.contains_key(r) {
                return Err(format!("`{path}.{r}` is required"));
            }
        }
        for (k, val) in o {
            match props.and_then(|p| p.get(k)) {
                Some(ps) => check(ps, val, &format!("{path}.{k}"), depth + 1)?,
                None => match s.get("additionalProperties") {
                    Some(Value::Bool(false)) => return Err(format!("`{path}` has no parameter `{k}`")),
                    Some(extra @ Value::Object(_)) => check(extra, val, &format!("{path}.{k}"), depth + 1)?,
                    _ => {}
                },
            }
        }
    }
    if let (Some(a), Some(items)) = (v.as_array(), s.get("items")) {
        for (i, item) in a.iter().enumerate() {
            check(items, item, &format!("{path}[{i}]"), depth + 1)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn checks_the_common_keywords() {
        let s = json!({"type": "object", "properties": {"id": {"type": "string"}, "n": {"type": "integer"}, "tags": {"type": "array", "items": {"enum": ["a", "b"]}}}, "required": ["id"], "additionalProperties": false});
        assert!(validate(&s, &json!({"id": "x", "n": 3, "tags": ["a"]})).is_ok());
        assert_eq!(validate(&s, &json!({})), Err("`arguments.id` is required".into()));
        assert_eq!(validate(&s, &json!({"id": 1})), Err("`arguments.id` must be string, got a number".into()));
        assert_eq!(validate(&s, &json!({"id": "x", "zz": 1})), Err("`arguments` has no parameter `zz`".into()));
        assert!(validate(&s, &json!({"id": "x", "tags": ["c"]})).is_err());
        assert!(validate(&s, &json!({"id": "x", "n": 2.5})).is_err());
        assert!(validate(&json!({"type": ["string", "null"]}), &Value::Null).is_ok());
        assert!(validate(&json!({}), &json!({"anything": true})).is_ok());
    }
}
