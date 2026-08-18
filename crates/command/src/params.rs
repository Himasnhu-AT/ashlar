//! Typed parameters: `const`-friendly declarations, validation and JSON Schema.

use serde_json::{Map, Value, json};

use crate::{CommandError, ParamKind, ParamSpec, Params, legacy};

/// Longest key quoted back in an error message (hostile params can carry huge keys).
const MAX_KEY_SHOWN: usize = 64;
/// At most this many unknown keys are named in one error.
const MAX_UNKNOWN_SHOWN: usize = 8;

/// The kind of a [`Param`]; the `const` counterpart of [`ParamKind`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    Bool,
    Int { min: Option<i64>, max: Option<i64> },
    Float { min: Option<f64>, max: Option<f64> },
    String,
    Enum(&'static [&'static str]),
    Json,
}

/// A typed parameter declared in a `const`/`static` command table:
/// `Param::int_in("width", 1, 16384).required().about("pixels")`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Param {
    pub name: &'static str,
    pub kind: Kind,
    pub required: bool,
    pub about: &'static str,
}

impl Param {
    pub const fn new(name: &'static str, kind: Kind) -> Self {
        Self { name, kind, required: false, about: "" }
    }
    pub const fn bool(name: &'static str) -> Self {
        Self::new(name, Kind::Bool)
    }
    pub const fn int(name: &'static str) -> Self {
        Self::new(name, Kind::Int { min: None, max: None })
    }
    pub const fn int_in(name: &'static str, min: i64, max: i64) -> Self {
        Self::new(name, Kind::Int { min: Some(min), max: Some(max) })
    }
    pub const fn float(name: &'static str) -> Self {
        Self::new(name, Kind::Float { min: None, max: None })
    }
    pub const fn float_in(name: &'static str, min: f64, max: f64) -> Self {
        Self::new(name, Kind::Float { min: Some(min), max: Some(max) })
    }
    pub const fn string(name: &'static str) -> Self {
        Self::new(name, Kind::String)
    }
    pub const fn one_of(name: &'static str, values: &'static [&'static str]) -> Self {
        Self::new(name, Kind::Enum(values))
    }
    pub const fn json(name: &'static str) -> Self {
        Self::new(name, Kind::Json)
    }
    pub const fn required(mut self) -> Self {
        self.required = true;
        self
    }
    pub const fn about(mut self, about: &'static str) -> Self {
        self.about = about;
        self
    }

    /// The owned, serializable form.
    pub fn to_spec(&self) -> ParamSpec {
        let kind = match self.kind {
            Kind::Bool => ParamKind::Bool,
            Kind::Int { min, max } => ParamKind::Int { min, max },
            Kind::Float { min, max } => ParamKind::Float { min, max },
            Kind::String => ParamKind::String,
            Kind::Enum(values) => ParamKind::Enum { values: values.iter().map(|v| (*v).to_string()).collect() },
            Kind::Json => ParamKind::Json,
        };
        ParamSpec { name: self.name.to_string(), kind, required: self.required, about: self.about.to_string() }
    }
}

/// Enum values of either declaration form.
#[derive(Clone, Copy)]
enum Values<'a> {
    Owned(&'a [String]),
    Static(&'a [&'static str]),
}

impl Values<'_> {
    fn contains(self, s: &str) -> bool {
        match self {
            Values::Owned(v) => v.iter().any(|x| x == s),
            Values::Static(v) => v.contains(&s),
        }
    }
    fn strings(self) -> Vec<String> {
        match self {
            Values::Owned(v) => v.to_vec(),
            Values::Static(v) => v.iter().map(|s| (*s).to_string()).collect(),
        }
    }
}

#[derive(Clone, Copy)]
enum KindView<'a> {
    Bool,
    Int(Option<i64>, Option<i64>),
    Float(Option<f64>, Option<f64>),
    Str,
    Enum(Values<'a>),
    Json,
}

/// One typed parameter, borrowed from either declaration form.
#[derive(Clone, Copy)]
struct View<'a> {
    name: &'a str,
    kind: KindView<'a>,
    required: bool,
    about: &'a str,
}

fn views(params: &Params) -> Option<Vec<View<'_>>> {
    match params {
        Params::None => Some(vec![]),
        Params::Text(_) => None,
        Params::Typed(specs) => Some(
            specs
                .iter()
                .map(|p| View {
                    name: &p.name,
                    kind: match &p.kind {
                        ParamKind::Bool => KindView::Bool,
                        ParamKind::Int { min, max } => KindView::Int(*min, *max),
                        ParamKind::Float { min, max } => KindView::Float(*min, *max),
                        ParamKind::String => KindView::Str,
                        ParamKind::Enum { values } => KindView::Enum(Values::Owned(values)),
                        ParamKind::Json => KindView::Json,
                    },
                    required: p.required,
                    about: &p.about,
                })
                .collect(),
        ),
        Params::Static(specs) => Some(
            specs
                .iter()
                .map(|p| View {
                    name: p.name,
                    kind: match p.kind {
                        Kind::Bool => KindView::Bool,
                        Kind::Int { min, max } => KindView::Int(min, max),
                        Kind::Float { min, max } => KindView::Float(min, max),
                        Kind::String => KindView::Str,
                        Kind::Enum(values) => KindView::Enum(Values::Static(values)),
                        Kind::Json => KindView::Json,
                    },
                    required: p.required,
                    about: p.about,
                })
                .collect(),
        ),
    }
}

pub(crate) fn shown(key: &str) -> String {
    match key.char_indices().nth(MAX_KEY_SHOWN) {
        Some((i, _)) => format!("`{}…`", key.get(..i).unwrap_or_default()),
        None => format!("`{key}`"),
    }
}

pub(crate) fn json_type(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

/// `unknown parameter(s) `a`, `b`; accepted: x, y`.
pub(crate) fn unknown_keys_error(unknown: &[&str], accepted: &[&str]) -> String {
    let mut names: Vec<String> = unknown.iter().take(MAX_UNKNOWN_SHOWN).map(|k| shown(k)).collect();
    if unknown.len() > MAX_UNKNOWN_SHOWN {
        names.push(format!("and {} more", unknown.len() - MAX_UNKNOWN_SHOWN));
    }
    let list = if accepted.is_empty() { "none".to_string() } else { accepted.join(", ") };
    format!("unknown parameter(s) {}; accepted: {list}", names.join(", "))
}

/// An integer value (JSON integer, or a float with no fractional part in the exact range).
fn as_int(v: &Value) -> Option<i64> {
    if let Some(i) = v.as_i64() {
        return Some(i);
    }
    let f = v.as_f64()?;
    // 2^53: past this a float no longer names one integer
    const EXACT: f64 = 9_007_199_254_740_992.0;
    (f.is_finite() && f.fract() == 0.0 && f.abs() <= EXACT).then_some(f as i64)
}

fn check_value(p: &View<'_>, v: &Value) -> Result<(), String> {
    let name = shown(p.name);
    match p.kind {
        KindView::Json => Ok(()),
        KindView::Bool => v.is_boolean().then_some(()).ok_or_else(|| format!("{name} must be true or false, got {}", json_type(v))),
        KindView::Str => v.is_string().then_some(()).ok_or_else(|| format!("{name} must be a string, got {}", json_type(v))),
        KindView::Enum(values) => {
            let all = values.strings().join(", ");
            match v.as_str() {
                Some(s) if values.contains(s) => Ok(()),
                Some(s) => Err(format!("{name} must be one of {all}, got {}", shown(s))),
                None => Err(format!("{name} must be one of {all}, got {}", json_type(v))),
            }
        }
        KindView::Int(min, max) => {
            let Some(i) = as_int(v) else {
                return Err(if v.is_number() {
                    format!("{name} must be a whole number, got {v}")
                } else {
                    format!("{name} must be a whole number, got {}", json_type(v))
                });
            };
            if let Some(m) = min.filter(|m| i < *m) {
                return Err(format!("{name} must be at least {m}, got {i}"));
            }
            if let Some(m) = max.filter(|m| i > *m) {
                return Err(format!("{name} must be at most {m}, got {i}"));
            }
            Ok(())
        }
        KindView::Float(min, max) => {
            let Some(f) = v.as_f64().filter(|f| f.is_finite()) else {
                return Err(format!("{name} must be a number, got {}", json_type(v)));
            };
            if let Some(m) = min.filter(|m| f < *m) {
                return Err(format!("{name} must be at least {m}, got {f}"));
            }
            if let Some(m) = max.filter(|m| f > *m) {
                return Err(format!("{name} must be at most {m}, got {f}"));
            }
            Ok(())
        }
    }
}

/// Check `value` against a command's declared params. Typed params: unknown keys, missing required
/// keys, types, ranges and enum values are all checked. Legacy text params: only keys the text
/// clearly does not mention are refused. `common` keys are accepted by every command; `aliases`
/// (`(documented, alias)`) let a legacy command accept another spelling.
pub fn validate_params(id: &str, params: &Params, value: &Value, common: &[&str], aliases: &[(&str, &str)]) -> Result<(), CommandError> {
    let Some(views) = views(params) else {
        return match params {
            Params::Text(doc) => legacy::check(doc, value, common, aliases).map_err(|msg| CommandError::bad_params(id, msg)),
            _ => Ok(()),
        };
    };
    let empty = Map::new();
    let obj = match value {
        Value::Null => &empty,
        Value::Object(m) => m,
        other => return Err(CommandError::bad_params(id, format!("expected an object of parameters, got {}", json_type(other)))),
    };
    let mut errors: Vec<String> = Vec::new();
    let unknown: Vec<&str> = obj.keys().map(String::as_str).filter(|k| !common.contains(k) && !views.iter().any(|p| p.name == *k)).collect();
    if !unknown.is_empty() {
        let accepted: Vec<&str> = views.iter().map(|p| p.name).collect();
        errors.push(unknown_keys_error(&unknown, &accepted));
    }
    for p in &views {
        match obj.get(p.name) {
            None | Some(Value::Null) if p.required => errors.push(format!("missing required parameter {}", shown(p.name))),
            None | Some(Value::Null) => {}
            Some(v) => {
                if let Err(e) = check_value(p, v) {
                    errors.push(e);
                }
            }
        }
    }
    if errors.is_empty() { Ok(()) } else { Err(CommandError::bad_params(id, errors.join("; "))) }
}

fn property(p: &View<'_>) -> Value {
    let mut m = Map::new();
    match p.kind {
        KindView::Bool => {
            m.insert("type".into(), "boolean".into());
        }
        KindView::Str => {
            m.insert("type".into(), "string".into());
        }
        KindView::Json => {}
        KindView::Enum(values) => {
            m.insert("type".into(), "string".into());
            m.insert("enum".into(), values.strings().into());
        }
        KindView::Int(min, max) => {
            m.insert("type".into(), "integer".into());
            if let Some(v) = min {
                m.insert("minimum".into(), v.into());
            }
            if let Some(v) = max {
                m.insert("maximum".into(), v.into());
            }
        }
        KindView::Float(min, max) => {
            m.insert("type".into(), "number".into());
            if let Some(v) = min {
                m.insert("minimum".into(), v.into());
            }
            if let Some(v) = max {
                m.insert("maximum".into(), v.into());
            }
        }
    }
    if !p.about.is_empty() {
        m.insert("description".into(), p.about.into());
    }
    Value::Object(m)
}

/// The JSON Schema (`{"type":"object",…}`) of a command's parameters (MCP tool schemas, CLI help,
/// inspector forms).
pub fn params_schema(params: &Params) -> Value {
    match views(params) {
        Some(views) => {
            let props: Map<String, Value> = views.iter().map(|p| (p.name.to_string(), property(p))).collect();
            let required: Vec<&str> = views.iter().filter(|p| p.required).map(|p| p.name).collect();
            json!({"type": "object", "properties": props, "required": required, "additionalProperties": false})
        }
        None => match params {
            Params::Text(doc) => legacy::schema(doc),
            _ => json!({"type": "object"}),
        },
    }
}
