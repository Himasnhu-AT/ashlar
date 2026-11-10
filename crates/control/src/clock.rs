//! `app.clock`: pause, step and resume the time a windowed app's frames see.
//!
//! Params, at most one of: `{pause: true}`, `{step_ms: N}` (pause if running, then run exactly one
//! frame N ms later), `{resume: true}`; `{}` only reads. The reply is `{paused, time}` (the egui
//! time the next frame sees, in seconds). Input still runs frames while paused, at the frozen
//! time. See `docs/control-protocol.md`.

use serde_json::Value;

/// The clock method. Answered by the windowed runtime (`ashlar-app`), which lists it in
/// `app.version`; a headless session refuses it.
pub const APP_CLOCK: &str = "app.clock";

/// Longest single `step_ms`: one minute.
pub const MAX_STEP_MS: f64 = 60_000.0;

/// What an `app.clock` request asks for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ClockOp {
    /// `{}`: report the clock.
    Read,
    /// `{pause: true}`: freeze time at the current frame's.
    Pause,
    /// `{step_ms: N}`: freeze (if running), then move the frozen time on by `N` ms for one frame.
    Step {
        /// Milliseconds, `0 < ms <= MAX_STEP_MS`.
        ms: f64,
    },
    /// `{resume: true}`: let time run again from the frozen value (no jump).
    Resume,
}

impl ClockOp {
    /// Read the request's params. `pause: false` and `resume: false` mean nothing; asking for two
    /// things at once is an error.
    pub fn from_params(p: &Value) -> Result<ClockOp, String> {
        if !p.is_null() && !p.is_object() {
            return Err("app.clock takes {pause: true}, {step_ms: N}, {resume: true} or {}".into());
        }
        let yes = |k: &str| p.get(k).and_then(Value::as_bool) == Some(true);
        let mut ops = Vec::new();
        if yes("pause") {
            ops.push(ClockOp::Pause);
        }
        if yes("resume") {
            ops.push(ClockOp::Resume);
        }
        if let Some(v) = p.get("step_ms").filter(|v| !v.is_null()) {
            let ms = v.as_f64().filter(|m| m.is_finite() && *m > 0.0 && *m <= MAX_STEP_MS);
            let Some(ms) = ms else { return Err(format!("step_ms is a number of milliseconds above 0 and at most {MAX_STEP_MS}")) };
            ops.push(ClockOp::Step { ms });
        }
        match ops.as_slice() {
            [] => Ok(ClockOp::Read),
            [op] => Ok(*op),
            _ => Err("app.clock does one of pause, step_ms or resume per request".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn params_name_one_operation() {
        assert_eq!(ClockOp::from_params(&json!({})), Ok(ClockOp::Read));
        assert_eq!(ClockOp::from_params(&Value::Null), Ok(ClockOp::Read));
        assert_eq!(ClockOp::from_params(&json!({"pause": true})), Ok(ClockOp::Pause));
        assert_eq!(ClockOp::from_params(&json!({"pause": false})), Ok(ClockOp::Read));
        assert_eq!(ClockOp::from_params(&json!({"resume": true})), Ok(ClockOp::Resume));
        assert_eq!(ClockOp::from_params(&json!({"step_ms": 16.7})), Ok(ClockOp::Step { ms: 16.7 }));
        assert!(ClockOp::from_params(&json!({"pause": true, "resume": true})).is_err());
        assert!(ClockOp::from_params(&json!({"step_ms": 0})).is_err());
        assert!(ClockOp::from_params(&json!({"step_ms": -5})).is_err());
        assert!(ClockOp::from_params(&json!({"step_ms": "16"})).is_err());
        assert!(ClockOp::from_params(&json!({"step_ms": 1e9})).is_err());
        assert!(ClockOp::from_params(&json!([1])).is_err());
    }
}
