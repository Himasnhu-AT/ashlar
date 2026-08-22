//! A tiny app for tests: a counter. Shared by the integration tests and the `stdio_worker` example.
#![allow(dead_code)]

use ashlar_command::{CommandError, CommandInfo, Dispatch};
use serde_json::{Value, json};

#[derive(Default)]
pub struct Counter(pub i64);

fn info(id: &str, label: &str, schema: Value, mutates: bool, enabled: Result<(), &str>) -> CommandInfo {
    CommandInfo {
        id: id.into(),
        label: label.into(),
        menu: vec!["Count".into()],
        shortcut: None,
        params_schema: schema,
        params_text: None,
        enabled: enabled.is_ok(),
        disabled_reason: enabled.err().map(String::from),
        mutates,
    }
}

impl Dispatch for Counter {
    fn commands(&self) -> Vec<CommandInfo> {
        vec![
            info(
                "count.add",
                "Add",
                json!({"type": "object", "properties": {"by": {"type": "integer", "description": "Amount (default 1)"}}, "required": []}),
                true,
                Ok(()),
            ),
            info("count.get", "Get", Value::Null, false, Ok(())),
            info("count.fail", "Fail", Value::Null, false, Ok(())),
            info("count.reset", "Reset", Value::Null, true, Err("already zero")),
            info("count.slow", "Slow", json!({"type": "object", "properties": {"ms": {"type": "integer"}}}), false, Ok(())),
            info("count.crash", "Crash", Value::Null, false, Ok(())),
        ]
    }

    fn execute(&mut self, id: &str, params: Value) -> Result<Value, CommandError> {
        match id {
            "count.add" => {
                let by = match params.get("by") {
                    None => 1,
                    Some(v) => v.as_i64().ok_or_else(|| CommandError::BadParams { id: id.into(), msg: "`by` must be an integer".into() })?,
                };
                self.0 = self.0.saturating_add(by);
                Ok(json!(self.0))
            }
            "count.get" => Ok(json!(self.0)),
            "count.fail" => Err(CommandError::Failed { id: id.into(), msg: "it always fails".into() }),
            "count.reset" if self.0 == 0 => Err(CommandError::Disabled { id: id.into(), reason: "already zero".into() }),
            "count.reset" => {
                self.0 = 0;
                Ok(json!(0))
            }
            "count.slow" => {
                let ms = params.get("ms").and_then(Value::as_u64).unwrap_or(0);
                std::thread::sleep(std::time::Duration::from_millis(ms));
                Ok(json!(self.0))
            }
            // A native library killing the process: only safe to call in a worker.
            "count.crash" => std::process::abort(),
            _ => Err(CommandError::Unknown { id: id.into() }),
        }
    }
}
