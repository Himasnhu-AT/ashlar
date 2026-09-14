//! Errors that say what to do instead: an unknown command names the closest commands and their
//! parameters, bad parameters name what the command takes. Used for `engine.execute` and
//! `engine.batch` here, and by the MCP server and CLIs for replies that come from elsewhere.

use ashlar_command::{CommandError, CommandInfo, did_you_mean_command, params_summary};
use serde_json::{Value, json};

use crate::protocol::Reply;

/// `detail` keys an explained error carries.
const SUGGESTIONS: &str = "suggestions";
const TAKES: &str = "takes";

/// A command's error as a reply, explained against `commands` (what the app lists). `detail`
/// keeps the error's `kind` and `id`; an unknown command adds `suggestions` ([{id, params}]) and
/// says "did you mean …" in the message, bad parameters add `takes` (the command's parameters)
/// and name them in the message unless it already lists them.
pub fn explain(e: &CommandError, commands: &[CommandInfo]) -> Reply {
    let mut detail = serde_json::to_value(e).unwrap_or_else(|_| json!({}));
    let message = explain_parts(e.to_string(), e, &mut detail, commands);
    Reply::Err { error: message, detail: Some(detail) }
}

/// [`explain`] for a reply that came from elsewhere (a running app, a worker): an error whose
/// `detail.kind` is `unknown` or `bad_params` (or whose message starts with "unknown command")
/// and that is not explained yet. `commands` is only called when there is something to explain.
/// Anything else comes back unchanged.
pub fn explain_reply(r: Reply, commands: impl FnOnce() -> Vec<CommandInfo>) -> Reply {
    let Reply::Err { error, detail } = r else { return r };
    let explained = detail.as_ref().is_some_and(|d| d.get(SUGGESTIONS).is_some() || d.get(TAKES).is_some()) || error.contains("did you mean");
    let parsed = detail.as_ref().and_then(|d| serde_json::from_value::<CommandError>(d.clone()).ok()).or_else(|| unknown_from_message(&error));
    match parsed {
        Some(e @ (CommandError::Unknown { .. } | CommandError::BadParams { .. })) if !explained => {
            let mut d = detail.unwrap_or_else(|| serde_json::to_value(&e).unwrap_or_else(|_| json!({})));
            if !d.is_object() {
                d = serde_json::to_value(&e).unwrap_or_else(|_| json!({}));
            }
            let message = explain_parts(error, &e, &mut d, &commands());
            Reply::Err { error: message, detail: Some(d) }
        }
        _ => Reply::Err { error, detail },
    }
}

/// "unknown command `x`" (as [`CommandError::Unknown`] prints) without a machine-readable detail.
fn unknown_from_message(error: &str) -> Option<CommandError> {
    let rest = error.split("unknown command `").nth(1)?;
    let id = rest.split('`').next()?;
    (!id.is_empty()).then(|| CommandError::Unknown { id: id.to_string() })
}

fn explain_parts(mut message: String, e: &CommandError, detail: &mut Value, commands: &[CommandInfo]) -> String {
    let Some(o) = detail.as_object_mut() else { return message };
    match e {
        CommandError::Unknown { id } => {
            let hits = ashlar_command::closest(id, commands.iter().map(|c| c.id.as_str()), 3);
            let list: Vec<Value> = hits
                .iter()
                .filter_map(|h| commands.iter().find(|c| c.id == *h))
                .map(|c| json!({"id": c.id, "label": c.label, "params": params_summary(c)}))
                .collect();
            o.insert(SUGGESTIONS.into(), Value::Array(list));
            match did_you_mean_command(id, commands) {
                Some(hint) => message.push_str(&format!("; {hint}")),
                None => message.push_str("; list the commands to find the right id"),
            }
        }
        CommandError::BadParams { id, msg } => {
            if let Some(c) = commands.iter().find(|c| c.id == *id) {
                let takes = params_summary(c);
                let listed = msg.contains("accepted:") || msg.contains("it takes") || msg.contains("takes:");
                if !listed {
                    message.push_str(&format!("; `{id}` takes {takes}"));
                }
                o.insert(TAKES.into(), json!(takes));
            }
        }
        _ => {}
    }
    message
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(id: &str, text: &str) -> CommandInfo {
        CommandInfo {
            id: id.into(),
            label: id.into(),
            menu: vec![],
            shortcut: None,
            params_schema: Value::Null,
            params_text: Some(text.into()),
            enabled: true,
            disabled_reason: None,
            mutates: true,
        }
    }

    fn list() -> Vec<CommandInfo> {
        vec![info("timeline.razor", "{time, track?}"), info("timeline.roll", "{left, right, delta}")]
    }

    #[test]
    fn unknown_commands_name_the_closest() {
        let Reply::Err { error, detail } = explain(&CommandError::Unknown { id: "timeline.razr".into() }, &list()) else { panic!() };
        assert_eq!(error, "unknown command `timeline.razr`; did you mean `timeline.razor` {time, track?}?");
        let d = detail.unwrap();
        assert_eq!((d["kind"].as_str(), d["id"].as_str()), (Some("unknown"), Some("timeline.razr")));
        assert_eq!(d["suggestions"][0]["id"], "timeline.razor");
        let Reply::Err { error, .. } = explain(&CommandError::Unknown { id: "qqqqq".into() }, &list()) else { panic!() };
        assert!(error.ends_with("list the commands to find the right id"), "{error}");
    }

    #[test]
    fn bad_params_name_what_the_command_takes() {
        let e = CommandError::bad_params("timeline.razor", "`time` is required");
        let Reply::Err { error, detail } = explain(&e, &list()) else { panic!() };
        assert_eq!(error, "invalid parameters for `timeline.razor`: `time` is required; `timeline.razor` takes {time, track?}");
        assert_eq!(detail.unwrap()["takes"], "{time, track?}");
        // a message that already lists them is left alone
        let e = CommandError::bad_params("timeline.razor", "unknown parameter(s) `x`; accepted: time, track");
        let Reply::Err { error, .. } = explain(&e, &list()) else { panic!() };
        assert!(!error.contains("takes"), "{error}");
    }

    #[test]
    fn replies_from_elsewhere_are_explained_once() {
        let calls = std::cell::Cell::new(0);
        let commands = || {
            calls.set(calls.get() + 1);
            list()
        };
        let r = Reply::from_command(Err(CommandError::Unknown { id: "timeline.rol".into() }));
        let once = explain_reply(r, commands);
        let Reply::Err { error, .. } = &once else { panic!() };
        assert!(error.contains("did you mean `timeline.roll`"), "{error}");
        let twice = explain_reply(once.clone(), list);
        assert_eq!(twice, once);
        // a message without detail
        let Reply::Err { error, .. } = explain_reply(Reply::err("unknown command `timeline.razer`"), list) else { panic!() };
        assert!(error.contains("`timeline.razor`"), "{error}");
        // other errors and successes pass through without listing the commands
        let failed = Reply::from_command(Err(CommandError::failed("timeline.razor", "no sequence")));
        assert_eq!(explain_reply(failed.clone(), commands), failed);
        assert_eq!(explain_reply(Reply::ok(json!(1)), commands), Reply::ok(json!(1)));
        assert_eq!(calls.get(), 1);
    }
}
