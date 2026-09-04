//! `batch`: a script of commands, one per line, with `assert` lines: the UI test runner.
//!
//! ```text
//! # comments and blank lines are skipped
//! snapshot -i
//! fill id=title "Groceries"
//! click button=Save
//! wait --text "Saved"
//! assert text id=status "Saved: Groceries"
//! assert contains id=status "Groceries"
//! assert enabled button=Delete
//! assert count role=listitem >= 3
//! assert exists role=button name~=Sav
//! ```
//!
//! Runs in order and stops at the first failure, naming the line, what was expected and what was
//! found. Assertions: `text|value|contains <T> <expected>`,
//! `visible|hidden|enabled|disabled|focused|checked|unchecked <T>`,
//! `count <selector…> <op> <n>` (op: `== != >= <= > <`), `exists|missing <selector…>`. A selector is
//! one or more `role=`, `name=`, `id=`, `value=` filters; `~=` matches a substring. `expect` is
//! `assert`'s twin; `echo <text>` prints; `sleep <ms>` waits.

use std::io::Read;
use std::time::Duration;

use serde_json::{Value, json};

use crate::words::{Args, quote, split};
use crate::{Ctx, Done, Fail, Res, target, usage_of};

/// Longest script read.
const MAX_SCRIPT: u64 = 16 * 1024 * 1024;

fn failed(error: String) -> Fail {
    Fail::Failed { error, detail: None, hint: None }
}

/// One element filter of a selector.
#[derive(Debug, Clone, PartialEq)]
struct Filter {
    key: String,
    value: String,
    contains: bool,
}

fn parse_selector(words: &[String]) -> Result<Vec<Filter>, String> {
    if words.is_empty() {
        return Err("a selector needs at least one filter, like role=button".into());
    }
    words
        .iter()
        .map(|w| {
            let (k, v, contains) = match (w.split_once("~="), w.split_once('=')) {
                (Some((k, v)), _) if !k.contains('=') => (k, v, true),
                (_, Some((k, v))) => (k, v, false),
                _ => return Err(format!("`{w}` is not a filter (role=…, name=…, id=…, value=…; ~= for a substring)")),
            };
            if !["role", "name", "id", "value"].contains(&k) {
                return Err(format!("unknown filter `{k}` (role, name, id, value)"));
            }
            Ok(Filter { key: k.into(), value: v.into(), contains })
        })
        .collect()
}

fn matches(e: &Value, filters: &[Filter]) -> bool {
    filters.iter().all(|f| {
        let got = e.get(&f.key).and_then(Value::as_str).unwrap_or("");
        if f.contains { got.contains(&f.value) } else { got == f.value }
    })
}

fn compare(n: u64, op: &str, want: u64) -> Option<bool> {
    Some(match op {
        "==" | "=" => n == want,
        "!=" => n != want,
        ">=" => n >= want,
        "<=" => n <= want,
        ">" => n > want,
        "<" => n < want,
        _ => return None,
    })
}

impl Ctx<'_, '_, '_> {
    pub(crate) fn batch(&mut self, mut a: Args) -> Res {
        let verbose = a.flag(&["--verbose", "-v"]);
        let file = a.positional().ok_or_else(|| Fail::Usage("batch needs a script (or - for stdin)".into(), usage_of("batch")))?;
        a.finish().map_err(|e| Fail::Usage(e, usage_of("batch")))?;
        if self.in_batch {
            return Err(failed("a batch cannot run another batch".into()));
        }
        let mut text = String::new();
        let read = if file == "-" {
            (&mut *self.io.input).take(MAX_SCRIPT).read_to_string(&mut text)
        } else {
            std::fs::File::open(&file).and_then(|f| f.take(MAX_SCRIPT).read_to_string(&mut text))
        };
        read.map_err(|e| Fail::Usage(format!("cannot read {file}: {e}"), usage_of("batch")))?;
        self.in_batch = true;
        let r = self.run_script(&file, &text, verbose);
        self.in_batch = false;
        r
    }

    fn run_script(&mut self, file: &str, text: &str, verbose: bool) -> Res {
        let mut steps = Vec::new();
        let mut passed = 0usize;
        for (i, line) in text.lines().enumerate() {
            let n = i.saturating_add(1);
            let words = match split(line) {
                Ok(w) if w.is_empty() => continue,
                Ok(w) => w,
                Err(e) => return Err(self.step_failed(file, n, line.trim(), &e, None, passed, steps)),
            };
            let shown = words.iter().map(|w| quote(w)).collect::<Vec<_>>().join(" ");
            let r = self.step(&words);
            match r {
                Ok(d) => {
                    passed = passed.saturating_add(1);
                    if verbose && !self.json {
                        let _ = writeln!(self.io.out, "ok   {n:>4}  {shown}");
                        for l in d.text.lines() {
                            let _ = writeln!(self.io.out, "            {l}");
                        }
                    }
                    steps.push(json!({"line": n, "command": shown, "ok": true, "result": d.result}));
                }
                Err(f) => {
                    let detail = match &f {
                        Fail::Failed { detail, .. } => detail.clone(),
                        Fail::Usage(..) => None,
                    };
                    return Err(self.step_failed(file, n, &shown, &f.text(), detail, passed, steps));
                }
            }
        }
        let text = format!("batch: {passed} steps passed ({file})");
        Ok(Done::new(json!({"passed": passed, "steps": steps}), text))
    }

    fn step_failed(
        &mut self,
        file: &str,
        line: usize,
        shown: &str,
        error: &str,
        detail: Option<Value>,
        passed: usize,
        mut steps: Vec<Value>,
    ) -> Fail {
        steps.push(json!({"line": line, "command": shown, "ok": false, "error": error}));
        if !self.json {
            let _ = writeln!(self.io.err, "FAIL {file}:{line}: {shown}\n     {error}");
        }
        Fail::Failed {
            error: format!("{file}:{line}: {error}"),
            detail: Some(json!({"line": line, "command": shown, "passed": passed, "steps": steps, "detail": detail})),
            hint: Some(format!("{passed} step(s) passed before line {line}")),
        }
    }

    /// One script line.
    fn step(&mut self, words: &[String]) -> Res {
        let Some((verb, rest)) = words.split_first() else { return Ok(Done::new(Value::Null, "")) };
        match verb.as_str() {
            "assert" | "expect" => self.assert(rest),
            "echo" => {
                let t = rest.join(" ");
                if !self.json {
                    let _ = writeln!(self.io.out, "{t}");
                }
                Ok(Done::new(json!(t), ""))
            }
            "sleep" => {
                let ms = rest.first().and_then(|m| m.parse::<u64>().ok()).ok_or_else(|| failed("sleep needs milliseconds".into()))?;
                std::thread::sleep(Duration::from_millis(ms.min(600_000)));
                Ok(Done::new(json!({"slept_ms": ms}), ""))
            }
            "open" | "connect" | "close" => {
                // A new target for the following lines.
                let r = self.run_words(words);
                if verb != "open" && verb != "connect" {
                    self.conn = None;
                }
                r
            }
            _ => self.run_words(words),
        }
    }

    /// Read one property of a target; `None` when the element is not there.
    fn read(&mut self, t: &str, what: &str) -> Result<Option<Value>, Fail> {
        let t = target::parse(t).map_err(failed)?;
        match self.call("ui.get", json!({"target": t, "what": what})) {
            Ok(v) => Ok(Some(v.get("value").cloned().unwrap_or(Value::Null))),
            Err(Fail::Failed { error, detail, .. })
                if error.starts_with("not_found")
                    || error.starts_with("stale_ref")
                    || detail.as_ref().and_then(|d| d.get("kind")).and_then(Value::as_str).is_some_and(|k| k == "not_found" || k == "stale_ref") =>
            {
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    fn elements(&mut self) -> Result<Vec<Value>, Fail> {
        let v = self.call("ui.snapshot", json!({}))?;
        Ok(v.get("elements").and_then(Value::as_array).cloned().unwrap_or_default())
    }

    fn assert(&mut self, w: &[String]) -> Res {
        let Some((kind, rest)) = w.split_first() else { return Err(failed("assert what? (text, visible, count, …)".into())) };
        let arg = |i: usize, what: &str| rest.get(i).cloned().ok_or_else(|| failed(format!("assert {kind} needs {what}")));
        match kind.as_str() {
            "text" | "value" | "contains" => {
                let (t, want) = (arg(0, "a target")?, arg(1, "the expected text")?);
                if rest.len() > 2 {
                    return Err(failed(format!("assert {kind}: quote the expected text (\"…\")")));
                }
                let what = if kind == "value" { "value" } else { "text" };
                let got = self.read(&t, what)?.ok_or_else(|| failed(format!("{t} is not there")))?;
                let got_s = match &got {
                    Value::String(s) => s.clone(),
                    Value::Null => String::new(),
                    v => v.to_string(),
                };
                let ok = if kind == "contains" { got_s.contains(&want) } else { got_s.trim() == want.trim() };
                if ok {
                    Ok(Done::new(json!({"got": got}), ""))
                } else if kind == "contains" {
                    Err(failed(format!("expected {t} to contain {}, got {}", quote(&want), quote(&got_s))))
                } else {
                    Err(failed(format!("expected {kind} of {t} to be {}, got {}", quote(&want), quote(&got_s))))
                }
            }
            "visible" | "hidden" | "enabled" | "disabled" | "focused" | "checked" | "unchecked" => {
                let t = arg(0, "a target")?;
                if rest.len() > 1 {
                    return Err(failed(format!("assert {kind} takes one target")));
                }
                let (what, want) = match kind.as_str() {
                    "visible" => ("visible", true),
                    "hidden" => ("visible", false),
                    "enabled" => ("enabled", true),
                    "disabled" => ("enabled", false),
                    "focused" => ("focused", true),
                    "checked" => ("checked", true),
                    _ => ("checked", false),
                };
                match self.read(&t, what)? {
                    None if kind == "hidden" => Ok(Done::new(json!({"present": false}), "")),
                    None => Err(failed(format!("expected {t} to be {kind}, but it is not there"))),
                    Some(v) if v.as_bool() == Some(want) => Ok(Done::new(json!({what: v}), "")),
                    Some(v) => Err(failed(format!("expected {t} to be {kind}, but {what} is {v}"))),
                }
            }
            "count" => {
                let op_at = rest
                    .iter()
                    .position(|x| compare(0, x, 0).is_some())
                    .ok_or_else(|| failed("assert count <selector…> <op> <n>, op one of == != >= <= > <".into()))?;
                let filters = parse_selector(rest.get(..op_at).unwrap_or(&[])).map_err(failed)?;
                let op = rest.get(op_at).cloned().unwrap_or_default();
                let want: u64 = rest
                    .get(op_at.saturating_add(1))
                    .and_then(|n| n.parse().ok())
                    .ok_or_else(|| failed(format!("assert count: a number after {op}")))?;
                if rest.len() > op_at.saturating_add(2) {
                    return Err(failed("assert count: unexpected words after the number".into()));
                }
                let n = self.elements()?.iter().filter(|e| matches(e, &filters)).count() as u64;
                if compare(n, &op, want) == Some(true) {
                    Ok(Done::new(json!({"count": n}), ""))
                } else {
                    Err(failed(format!("expected count {op} {want}, got {n}")))
                }
            }
            "exists" | "missing" => {
                let filters = parse_selector(rest).map_err(failed)?;
                let n = self.elements()?.iter().filter(|e| matches(e, &filters)).count();
                match (kind.as_str(), n) {
                    ("exists", 0) => Err(failed(format!("expected an element matching {}, found none", rest.join(" ")))),
                    ("missing", n) if n > 0 => Err(failed(format!("expected no element matching {}, found {n}", rest.join(" ")))),
                    _ => Ok(Done::new(json!({"count": n}), "")),
                }
            }
            other => Err(failed(format!(
                "unknown assertion `{other}` (text, value, contains, visible, hidden, enabled, disabled, focused, checked, unchecked, count, exists, missing)"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selectors() {
        let f = parse_selector(&["role=button".into(), "name~=Sav".into()]).unwrap();
        assert!(matches(&json!({"role": "button", "name": "Save"}), &f));
        assert!(!matches(&json!({"role": "button", "name": "Load"}), &f));
        assert!(!matches(&json!({"role": "label", "name": "Save"}), &f));
        assert!(parse_selector(&[]).is_err());
        assert!(parse_selector(&["colour=red".into()]).is_err());
        assert!(parse_selector(&["button".into()]).is_err());
        // `=` inside a value is fine.
        assert_eq!(parse_selector(&["name=a=b".into()]).unwrap()[0].value, "a=b");
    }

    #[test]
    fn comparisons() {
        assert_eq!(compare(3, ">=", 3), Some(true));
        assert_eq!(compare(2, ">", 3), Some(false));
        assert_eq!(compare(2, "!=", 3), Some(true));
        assert_eq!(compare(2, "~", 3), None);
    }
}
