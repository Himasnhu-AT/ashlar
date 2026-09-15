#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Typed params: validation messages and JSON Schema output (exact snapshots).

use ashlar_command::{CommandError, CommandSpec, Host, Journal, Kind, Param, ParamKind, ParamSpec, Params, Registry, always, cmd, query};
use serde_json::{Value, json};

#[derive(Default)]
struct Doc {
    journal: Journal,
    width: i64,
}

impl Host for Doc {
    type Error = CommandError;
    fn journal(&mut self) -> &mut Journal {
        &mut self.journal
    }
}

const RESIZE: &[Param] = &[
    Param::int_in("width", 1, 16_384).required().about("pixels"),
    Param::int_in("height", 1, 16_384).required(),
    Param::float_in("scale", 0.01, 100.0),
    Param::one_of("fit", &["contain", "cover", "stretch"]),
    Param::bool("keepAspect"),
    Param::string("note"),
    Param::json("meta"),
];

fn registry() -> Registry<Doc> {
    Registry::new(vec![
        CommandSpec::new("image.resize", "Resize…", |d: &mut Doc, p: &Value| {
            d.width = p["width"].as_i64().unwrap_or_default();
            Ok(Value::Null)
        })
        .menu(&["Image"])
        .shortcut("Mod+Alt+I")
        .params(Params::Static(RESIZE)),
        cmd!("image.rotate", "Rotate", ["Image"], None, [Param::float("degrees").required()], always, |_, _| Ok(Value::Null)),
        query!("image.info", "Info", [], |d, _| Ok(json!({"width": d.width}))),
        CommandSpec::new("image.flip", "Flip", |_: &mut Doc, _: &Value| Ok(Value::Null)).params(Params::Typed(vec![
            ParamSpec::new("axis", ParamKind::Enum { values: vec!["x".into(), "y".into()] }).required().about("mirror axis"),
        ])),
        CommandSpec::new("image.reset", "Reset", |_: &mut Doc, _: &Value| Ok(Value::Null)),
    ])
}

fn bad(r: Result<Value, CommandError>) -> String {
    match r {
        Err(CommandError::BadParams { msg, .. }) => msg,
        other => panic!("expected BadParams, got {other:?}"),
    }
}

#[test]
fn valid_params_run() {
    let reg = registry();
    let mut d = Doc::default();
    let p = json!({"width": 640, "height": 480.0, "scale": 2, "fit": "cover", "keepAspect": true, "note": "x", "meta": [1, {"a": 2}]});
    assert_eq!(reg.execute(&mut d, "image.resize", p), Ok(Value::Null));
    assert_eq!(d.width, 640);
    // optional keys may be absent or null
    assert!(reg.execute(&mut d, "image.resize", json!({"width": 1, "height": 1, "fit": null})).is_ok());
    assert!(reg.execute(&mut d, "image.info", Value::Null).is_ok());
}

#[test]
fn every_problem_is_reported_at_once() {
    let reg = registry();
    let mut d = Doc::default();
    let msg = bad(reg.execute(&mut d, "image.resize", json!({"widht": 1, "height": 0, "scale": "big", "fit": "fill", "keepAspect": 1})));
    assert_eq!(
        msg,
        "unknown parameter(s) `widht` (did you mean `width`?); accepted: width, height, scale, fit, keepAspect, note, meta; \
         missing required parameter `width`; `height` must be at least 1, got 0; `scale` must be a number, got a string; \
         `fit` must be one of contain, cover, stretch, got `fill`; `keepAspect` must be true or false, got a number"
    );
    assert_eq!(d.width, 0, "nothing ran");
}

#[test]
fn numbers_are_checked_strictly() {
    let reg = registry();
    let mut d = Doc::default();
    let m = |p| bad(reg.execute(&mut Doc::default(), "image.resize", p));
    assert_eq!(m(json!({"width": 1.5, "height": 1})), "`width` must be a whole number, got 1.5");
    assert_eq!(m(json!({"width": 1e300, "height": 1})), "`width` must be a whole number, got 1e+300");
    assert_eq!(m(json!({"width": 99_999, "height": 1})), "`width` must be at most 16384, got 99999");
    assert_eq!(m(json!({"width": u64::MAX, "height": 1})), "`width` must be a whole number, got 18446744073709551615");
    assert_eq!(m(json!({"width": 2, "height": 2, "scale": 0.001})), "`scale` must be at least 0.01, got 0.001");
    assert_eq!(bad(reg.execute(&mut d, "image.resize", json!([1, 2]))), "expected an object of parameters, got an array");
    assert_eq!(bad(reg.execute(&mut d, "image.reset", json!({"x": 1}))), "unknown parameter(s) `x`; accepted: none");
    assert_eq!(bad(reg.execute(&mut d, "image.flip", json!({"axis": "z"}))), "`axis` must be one of x, y, got `z`");
}

#[test]
fn hostile_keys_are_truncated_and_counted() {
    let reg = registry();
    let mut obj = serde_json::Map::new();
    for i in 0..1000 {
        obj.insert(format!("{}{i}", "k".repeat(500)), json!(i));
    }
    let msg = bad(reg.execute(&mut Doc::default(), "image.reset", Value::Object(obj)));
    assert!(msg.len() < 1_000, "{}", msg.len());
    assert!(msg.contains("and 992 more"), "{msg}");
}

#[test]
fn schema_snapshots() {
    let reg = registry();
    assert_eq!(
        reg.schema("image.resize").unwrap(),
        &json!({
            "type": "object",
            "properties": {
                "width": {"type": "integer", "minimum": 1, "maximum": 16384, "description": "pixels"},
                "height": {"type": "integer", "minimum": 1, "maximum": 16384},
                "scale": {"type": "number", "minimum": 0.01, "maximum": 100.0},
                "fit": {"type": "string", "enum": ["contain", "cover", "stretch"]},
                "keepAspect": {"type": "boolean"},
                "note": {"type": "string"},
                "meta": {}
            },
            "required": ["width", "height"],
            "additionalProperties": false
        })
    );
    assert_eq!(
        reg.schema("image.flip").unwrap(),
        &json!({
            "type": "object",
            "properties": {"axis": {"type": "string", "enum": ["x", "y"], "description": "mirror axis"}},
            "required": ["axis"],
            "additionalProperties": false
        })
    );
    assert_eq!(reg.schema("image.reset").unwrap(), &json!({"type": "object", "properties": {}, "required": [], "additionalProperties": false}));
    assert_eq!(
        ashlar_command::params_schema(&Params::Text(r#"{"name":str,"fps":f64=23.976,"mix":"Stereo|Mono"?}"#)),
        json!({
            "type": "object",
            "description": r#"{"name":str,"fps":f64=23.976,"mix":"Stereo|Mono"?}"#,
            "properties": {
                "name": {"type": "string", "description": r#""name":str"#},
                "fps": {"type": "number", "description": r#""fps":f64=23.976"#},
                "mix": {"type": "string", "enum": ["Stereo", "Mono"], "description": r#""mix":"Stereo|Mono"?"#}
            },
            "required": ["name"],
            "additionalProperties": false
        })
    );
    assert_eq!(
        ashlar_command::params_schema(&Params::Text("any JSON the importer understands")),
        json!({"type": "object", "description": "any JSON the importer understands"})
    );
}

#[test]
fn static_and_owned_params_agree() {
    let owned: Vec<ParamSpec> = RESIZE.iter().map(Param::to_spec).collect();
    assert_eq!(ashlar_command::params_schema(&Params::Typed(owned.clone())), ashlar_command::params_schema(&Params::Static(RESIZE)));
    assert_eq!(owned[3].kind, ParamKind::Enum { values: vec!["contain".into(), "cover".into(), "stretch".into()] });
    assert_eq!(RESIZE[0].kind, Kind::Int { min: Some(1), max: Some(16_384) });
    // the contract type round-trips through JSON (MCP / control port)
    let back: Vec<ParamSpec> = serde_json::from_value(serde_json::to_value(&owned).unwrap()).unwrap();
    assert_eq!(back, owned);
}

#[test]
fn info_carries_schema_and_typed_specs_have_no_text() {
    let reg = registry();
    let d = Doc::default();
    let info = reg.info(reg.find("image.resize").unwrap(), &d);
    assert_eq!(info.shortcut.as_deref(), Some("Mod+Alt+I"));
    assert_eq!(info.params_text, None);
    assert_eq!(info.params_schema["required"], json!(["width", "height"]));
    assert!(!reg.find("image.info").unwrap().mutates);
}
