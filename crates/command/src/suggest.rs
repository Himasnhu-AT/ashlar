//! "Did you mean …?": the names closest to a mistyped one (a command id, a parameter key), and a
//! one-line summary of a command's parameters, so an error can say what to call instead.

use serde_json::Value;

use crate::{CommandInfo, fuzzy_match};

/// Names longer than this are not compared (bounded work on hostile input).
const MAX_NAME: usize = 128;
/// Longest parameter summary [`params_summary`] returns.
const MAX_SUMMARY: usize = 240;

/// Up to `max` of `candidates` closest to `name`, best first: a different case, then a small
/// spelling distance (at most a third of the name's length, at least 2), then names that share
/// the last dotted segment (`layer.delete` → `edit.delete`) or contain the name, then names that
/// hold its letters in order. Empty when nothing is close.
pub fn closest<'a>(name: &str, candidates: impl IntoIterator<Item = &'a str>, max: usize) -> Vec<&'a str> {
    let wanted = name.trim();
    if wanted.is_empty() || wanted.chars().count() > MAX_NAME || max == 0 {
        return Vec::new();
    }
    let lower = wanted.to_lowercase();
    let len = lower.chars().count();
    let limit = (len / 3).max(2);
    let last = lower.rsplit('.').next().unwrap_or(&lower).to_string();
    let mut ranked: Vec<(u8, usize, i64, &'a str)> = Vec::new();
    for c in candidates {
        if c == wanted || c.chars().count() > MAX_NAME {
            continue;
        }
        let cl = c.to_lowercase();
        let d = distance(&lower, &cl);
        let tier_score = if d <= limit {
            Some((0, d, 0))
        } else if (last.chars().count() >= 3 && cl.rsplit('.').next() == Some(last.as_str()))
            || (len >= 3 && (cl.contains(&lower) || (cl.chars().count() >= 3 && lower.contains(&cl))))
        {
            Some((1, d, 0))
        } else if len >= 3 {
            fuzzy_match(&lower, &cl).map(|m| (2, d, -i64::from(m.score)))
        } else {
            None
        };
        if let Some((tier, d, score)) = tier_score {
            ranked.push((tier, d, score, c));
        }
    }
    ranked.sort();
    ranked.dedup_by(|a, b| a.3 == b.3);
    // a near-certain match (one or two edits) is not diluted by names that are merely similar
    if let Some(&(0, best, _, _)) = ranked.first()
        && best <= 2
    {
        ranked.retain(|r| r.0 == 0 && r.1 <= best.saturating_add(1));
    }
    ranked.into_iter().take(max).map(|r| r.3).collect()
}

/// Edit distance (insertions, deletions, substitutions; a swap of two neighbours counts once).
fn distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().take(MAX_NAME).collect();
    let b: Vec<char> = b.chars().take(MAX_NAME).collect();
    // three rows of the optimal-string-alignment table
    let mut before: Vec<usize> = Vec::new();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut row = vec![i.saturating_add(1); b.len().saturating_add(1)];
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            let del = prev.get(j.saturating_add(1)).copied().unwrap_or(usize::MAX).saturating_add(1);
            let ins = row.get(j).copied().unwrap_or(usize::MAX).saturating_add(1);
            let sub = prev.get(j).copied().unwrap_or(usize::MAX).saturating_add(cost);
            let mut best = del.min(ins).min(sub);
            let swapped = i > 0 && j > 0 && a.get(i - 1) == Some(cb) && b.get(j - 1) == Some(ca);
            if swapped && let Some(t) = before.get(j - 1) {
                best = best.min(t.saturating_add(1));
            }
            if let Some(cell) = row.get_mut(j.saturating_add(1)) {
                *cell = best;
            }
        }
        before = std::mem::replace(&mut prev, row);
    }
    prev.last().copied().unwrap_or(0)
}

/// A command's parameters on one line: its text doc as written, or the keys of its schema
/// (`{clip, time?, seconds?}`, optional ones marked `?`); `{}` when it takes none. Long docs are
/// cut at about 240 characters.
pub fn params_summary(info: &CommandInfo) -> String {
    if let Some(text) = info.params_text.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        return cut(text);
    }
    let Some(props) = info.params_schema.get("properties").and_then(Value::as_object) else { return "{}".into() };
    let required: Vec<&str> = info.params_schema.get("required").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).collect();
    let keys: Vec<String> = props.keys().map(|k| if required.contains(&k.as_str()) { k.clone() } else { format!("{k}?") }).collect();
    cut(&format!("{{{}}}", keys.join(", ")))
}

fn cut(text: &str) -> String {
    match text.char_indices().nth(MAX_SUMMARY) {
        Some((i, _)) => format!("{}…", text.get(..i).unwrap_or_default()),
        None => text.to_string(),
    }
}

/// `did you mean `a` {params}, `b` {params}?` for an unknown command id, from the commands an app
/// lists; `None` when no command is close.
pub fn did_you_mean_command(id: &str, commands: &[CommandInfo]) -> Option<String> {
    let hits = closest(id, commands.iter().map(|c| c.id.as_str()), 3);
    if hits.is_empty() {
        return None;
    }
    let named: Vec<String> = hits
        .iter()
        .filter_map(|h| commands.iter().find(|c| c.id == *h))
        .map(|c| {
            let p = params_summary(c);
            if p == "{}" { format!("`{}`", c.id) } else { format!("`{}` {p}", c.id) }
        })
        .collect();
    Some(format!("did you mean {}?", named.join(", ")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn close_names_come_first() {
        let ids = ["timeline.razor", "timeline.roll", "timeline.move", "edit.clear", "layer.addMask", "layer.addTextAnimator"];
        assert_eq!(closest("timeline.razr", ids, 3).first(), Some(&"timeline.razor"));
        assert_eq!(closest("Timeline.Razor", ids, 1), vec!["timeline.razor"]);
        assert_eq!(closest("timeline.mvoe", ids, 1), vec!["timeline.move"], "a swap counts once");
        assert!(closest("layer.adx", ids, 3).iter().all(|c| c.starts_with("layer.add")), "{:?}", closest("layer.adx", ids, 3));
        assert_eq!(closest("clear", ids, 3), vec!["edit.clear"]);
        assert!(closest("zzzzzz", ids, 3).is_empty());
        assert!(closest("", ids, 3).is_empty());
        assert!(closest("timeline.razor", ids, 3).iter().all(|c| *c != "timeline.razor"), "the name itself is not a suggestion");
        let long = "x".repeat(10_000);
        assert!(closest(&long, ids, 3).is_empty());
        assert_eq!(closest("sekonds", ["seconds", "frame", "timecode"], 1), vec!["seconds"]);
    }

    fn info(id: &str, schema: Value, text: Option<&str>) -> CommandInfo {
        CommandInfo {
            id: id.into(),
            label: id.into(),
            menu: vec![],
            shortcut: None,
            params_schema: schema,
            params_text: text.map(String::from),
            enabled: true,
            disabled_reason: None,
            mutates: true,
        }
    }

    #[test]
    fn summaries_and_suggestions() {
        let typed = info("count.add", json!({"type": "object", "properties": {"by": {}, "to": {}}, "required": ["to"]}), None);
        assert_eq!(params_summary(&typed), "{by?, to}");
        let text = info("timeline.razor", json!({}), Some(r#"{"time":ticks,"track":"V1"|id?}"#));
        assert_eq!(params_summary(&text), r#"{"time":ticks,"track":"V1"|id?}"#);
        let none = info("edit.undo", Value::Null, None);
        assert_eq!(params_summary(&none), "{}");
        let long = info("x.y", json!({}), Some(&"a".repeat(1000)));
        assert!(params_summary(&long).chars().count() <= MAX_SUMMARY + 1);
        let all = [typed, text, none];
        assert_eq!(did_you_mean_command("timeline.razr", &all).as_deref(), Some(r#"did you mean `timeline.razor` {"time":ticks,"track":"V1"|id?}?"#));
        assert_eq!(did_you_mean_command("edit.undoo", &all).as_deref(), Some("did you mean `edit.undo`?"));
        assert_eq!(did_you_mean_command("qqqqqq", &all), None);
    }
}
