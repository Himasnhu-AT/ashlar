//! Wire types of the control protocol: one JSON object per line each way.
//!
//! Request: `{"id"?: any, "key"?: string, "method": string, "params"?: any}`.
//! Reply: `{"id"?: any, "ok": true, "result": any}` or `{"id"?: any, "ok": false, "error": string, "detail"?: any}`.
//! See `docs/control-protocol.md`.

use std::sync::mpsc::{Receiver, Sender, channel};

use ashlar_command::CommandError;
use serde_json::{Map, Value, json};

/// Version of the method vocabulary, reported by `app.version`. Bumped on incompatible changes.
pub const PROTOCOL_VERSION: u32 = 1;

/// One reply. `detail` carries machine-readable context for an error (a [`CommandError`] as JSON,
/// the step results of a failed batch).
#[derive(Debug, Clone, PartialEq)]
pub enum Reply {
    Ok(Value),
    Err { error: String, detail: Option<Value> },
}

impl Reply {
    pub fn ok(v: Value) -> Self {
        Reply::Ok(v)
    }

    pub fn err(e: impl std::fmt::Display) -> Self {
        Reply::Err { error: e.to_string(), detail: None }
    }

    pub fn err_with(e: impl std::fmt::Display, detail: Value) -> Self {
        Reply::Err { error: e.to_string(), detail: Some(detail) }
    }

    /// A command's outcome; errors keep their kind in `detail`.
    pub fn from_command(r: Result<Value, CommandError>) -> Self {
        match r {
            Ok(v) => Reply::Ok(v),
            Err(e) => Reply::Err { error: e.to_string(), detail: serde_json::to_value(&e).ok() },
        }
    }

    pub fn is_ok(&self) -> bool {
        matches!(self, Reply::Ok(_))
    }

    /// The wire object, with `id` when the request had one.
    pub fn to_json(&self, id: Option<&Value>) -> Value {
        let mut o = Map::new();
        if let Some(id) = id {
            o.insert("id".into(), id.clone());
        }
        match self {
            Reply::Ok(v) => {
                o.insert("ok".into(), Value::Bool(true));
                o.insert("result".into(), v.clone());
            }
            Reply::Err { error, detail } => {
                o.insert("ok".into(), Value::Bool(false));
                o.insert("error".into(), Value::String(error.clone()));
                if let Some(d) = detail {
                    o.insert("detail".into(), d.clone());
                }
            }
        }
        Value::Object(o)
    }

    /// Read a wire reply. A missing or non-boolean `ok` is a protocol error (`None`).
    pub fn from_json(v: &Value) -> Option<Self> {
        match v.get("ok")?.as_bool()? {
            true => Some(Reply::Ok(v.get("result").cloned().unwrap_or(Value::Null))),
            false => {
                Some(Reply::Err { error: v.get("error").and_then(Value::as_str).unwrap_or("error").to_string(), detail: v.get("detail").cloned() })
            }
        }
    }

    /// `Ok(result)` or `Err((error, detail))`.
    pub fn into_result(self) -> Result<Value, (String, Option<Value>)> {
        match self {
            Reply::Ok(v) => Ok(v),
            Reply::Err { error, detail } => Err((error, detail)),
        }
    }
}

/// One request on its way to the app thread. Answer it with [`Request::reply`] (or a shortcut);
/// a request dropped unanswered replies with an error by itself, so the connection never hangs.
/// It may be kept and answered on a later frame (the element is not on screen yet, a screenshot
/// needs the next paint).
#[derive(Debug)]
pub struct Request {
    method: String,
    params: Value,
    slot: Option<Sender<Reply>>,
}

impl Request {
    /// A request and the receiver its reply arrives on. `params` that are `null` become `{}`.
    pub fn new(method: impl Into<String>, params: Value) -> (Self, Receiver<Reply>) {
        let (tx, rx) = channel();
        let params = if params.is_null() { json!({}) } else { params };
        (Self { method: method.into(), params, slot: Some(tx) }, rx)
    }

    pub fn method(&self) -> &str {
        &self.method
    }

    pub fn params(&self) -> &Value {
        &self.params
    }

    /// Answer. The receiver may be gone (the connection closed or timed out): that is fine.
    pub fn reply(mut self, r: Reply) {
        if let Some(tx) = self.slot.take() {
            let _ = tx.send(r);
        }
    }

    pub fn ok(self, v: Value) {
        self.reply(Reply::Ok(v));
    }

    pub fn fail(self, e: impl std::fmt::Display) {
        self.reply(Reply::err(e));
    }

    /// The standard answer for a method nobody handles.
    pub fn unknown(self) {
        let m = self.method.clone();
        self.fail(format!("unknown method `{m}` (see app.version for the methods this app answers)"));
    }
}

impl Drop for Request {
    fn drop(&mut self) {
        if let Some(tx) = self.slot.take() {
            let _ = tx.send(Reply::err(format!("`{}` was dropped by the app without an answer", self.method)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reply_round_trips() {
        for r in [Reply::ok(json!({"a": 1})), Reply::err("nope"), Reply::err_with("bad", json!({"steps": []}))] {
            let wire = r.to_json(Some(&json!(7)));
            assert_eq!(wire["id"], 7);
            assert_eq!(Reply::from_json(&wire), Some(r));
        }
        assert_eq!(Reply::ok(Value::Null).to_json(None), json!({"ok": true, "result": null}));
        assert_eq!(Reply::from_json(&json!({"result": 1})), None);
        assert_eq!(Reply::from_json(&json!({"ok": "yes"})), None);
    }

    #[test]
    fn command_errors_keep_their_kind() {
        let r = Reply::from_command(Err(CommandError::Unknown { id: "x".into() }));
        let Reply::Err { error, detail } = r else { panic!() };
        assert_eq!(error, "unknown command `x`");
        assert_eq!(detail, Some(json!({"kind": "unknown", "id": "x"})));
    }

    #[test]
    fn a_dropped_request_answers_itself() {
        let (req, rx) = Request::new("ui.click", Value::Null);
        assert_eq!(req.params(), &json!({}));
        drop(req);
        let r = rx.try_recv().unwrap();
        assert!(!r.is_ok());
        // Answered once only.
        let (req, rx) = Request::new("x", json!({}));
        req.ok(json!(1));
        assert_eq!(rx.try_recv().unwrap(), Reply::ok(json!(1)));
        assert!(rx.try_recv().is_err());
    }
}
