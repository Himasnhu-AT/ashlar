//! Versioned document files: an envelope, a migration chain, and durable saves.
//!
//! A file is UTF-8 JSON:
//!
//! ```json
//! { "format": "acme.drawing", "schema_version": 3, "generator": "Acme Draw 1.2.0", "document": { … } }
//! ```
//!
//! - **Versioning.** The schema this build writes is `migrations.len() + 1`. Older files are
//!   upgraded on load by single-step migrations (`migrations[i]` upgrades `i + 1` to `i + 2`)
//!   operating on the whole JSON envelope, so each historical shape needs one small function.
//!   Append one per schema bump and never edit a shipped one; a bump that only adds fields is a
//!   no-op step, and still worth it: older builds then refuse the file instead of silently dropping
//!   the new data when they save.
//! - **Refusals.** A file from a newer schema is refused ([`FormatError::TooNew`]) instead of being
//!   half-read and later overwritten; a file of another format is refused
//!   ([`FormatError::WrongFormat`]).
//! - **Pre-envelope files.** [`Format::legacy`] recognises bare documents written before the
//!   envelope existed and says which schema they are; the first migration then wraps them.
//! - **Other envelope keys** (state outside the schema's promises, such as which views were open)
//!   are kept in [`Loaded::extras`] and written back by [`Format::encode_with`].
//!   They are written before the payload key, or after it with [`Format::extras_after_payload`]
//!   (so files written as `{format, schema_version, generator, payload, extras…}` re-encode
//!   byte for byte).
//! - **Untagged envelopes.** [`Format::accept_untagged`] reads envelopes without a `format` tag
//!   (written by early builds) when they have `schema_version` and the payload key.
//! - **Parse errors** keep serde's message and position: [`FormatError::json`].
//! - **Saves** go through [`ashlar_fs::atomic_write`].

use std::fmt;
use std::marker::PhantomData;
use std::path::Path;

use serde::de::{DeserializeOwned, DeserializeSeed, IgnoredAny, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};

/// Upgrades a whole file from schema `n` to `n + 1`.
pub type Migration = fn(Value) -> Result<Value, String>;

/// Recognises a pre-envelope document and returns its schema version.
pub type LegacyProbe = fn(&Value) -> Option<u64>;

/// Largest file [`Format::load`] reads unless set with [`Format::max_bytes`].
pub const DEFAULT_MAX_BYTES: u64 = 2 << 30;

const FORMAT: &str = "format";
const SCHEMA: &str = "schema_version";
const GENERATOR: &str = "generator";

#[derive(Debug, thiserror::Error)]
pub enum FormatError {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    /// For malformed or truncated JSON the message is serde's, position included.
    #[error("the file is damaged: {0}")]
    Corrupt(String),
    #[error("not a {expected} file: {why}")]
    NotThisFormat {
        expected: String,
        why: String,
        /// The serde error behind the refusal, when there was one (`why` is then its message).
        detail: Option<JsonError>,
    },
    #[error("this is a `{found}` file, not `{expected}`")]
    WrongFormat { found: String, expected: String },
    #[error("this file was saved by a newer version (schema v{found}); this build reads up to v{supported}. Update to open it.")]
    TooNew { found: u64, supported: u64 },
    #[error("schema v{found} is too old for this build (oldest supported: v{oldest})")]
    TooOld { found: u64, oldest: u64 },
    #[error("upgrading from schema v{from} to v{}: {msg}", from.saturating_add(1))]
    Migration { from: u64, msg: String },
    #[error("could not encode the document: {0}")]
    Encode(String),
}

/// A serde_json error as the parser reported it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JsonError {
    /// serde's message, position included (`"… at line 3 column 7"`).
    pub message: String,
    /// 1-based; 0 when serde has no position.
    pub line: usize,
    pub column: usize,
    /// Malformed or truncated JSON, as opposed to valid JSON of the wrong shape.
    pub syntax: bool,
}

impl JsonError {
    pub fn new(e: &serde_json::Error) -> JsonError {
        JsonError { message: e.to_string(), line: e.line(), column: e.column(), syntax: e.is_syntax() || e.is_eof() }
    }
}

impl FormatError {
    /// The serde error behind a [`FormatError::NotThisFormat`], with its position. (A
    /// [`FormatError::Corrupt`] from malformed JSON is serde's message verbatim, position included.)
    pub fn json(&self) -> Option<&JsonError> {
        match self {
            FormatError::NotThisFormat { detail, .. } => detail.as_ref(),
            _ => None,
        }
    }
}

/// The envelope fields of a file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    pub format: String,
    pub schema_version: u64,
    pub generator: Option<String>,
}

/// A document read from a file.
#[derive(Debug)]
pub struct Loaded<D> {
    pub doc: D,
    /// The envelope as found in the file (`schema_version` before migration).
    pub envelope: Envelope,
    /// Other top-level keys of the envelope (after migration).
    pub extras: Map<String, Value>,
}

impl<D> Loaded<D> {
    /// The file was in an older schema and was upgraded on load (saving it rewrites it in the
    /// current schema, which older builds can't read: worth telling the user).
    pub fn migrated(&self, current: u64) -> bool {
        self.envelope.schema_version < current
    }
}

/// How documents of type `D` are stored.
pub struct Format<D> {
    id: String,
    generator: String,
    payload_key: String,
    migrations: Vec<Migration>,
    legacy: Option<LegacyProbe>,
    oldest: u64,
    pretty: bool,
    max_bytes: u64,
    extras_after_payload: bool,
    accept_untagged: bool,
    _doc: PhantomData<fn() -> D>,
}

impl<D> fmt::Debug for Format<D> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Format").field("id", &self.id).field("schema_version", &(self.migrations.len() as u64 + 1)).finish()
    }
}

impl<D: Serialize + DeserializeOwned> Format<D> {
    /// Files tagged `id` (e.g. `"acme.drawing"`), written by `generator` (e.g. `"Acme Draw 1.2.0"`).
    pub fn new(id: impl Into<String>, generator: impl Into<String>) -> Format<D> {
        Format {
            id: id.into(),
            generator: generator.into(),
            payload_key: "document".into(),
            migrations: Vec::new(),
            legacy: None,
            oldest: 1,
            pretty: true,
            max_bytes: DEFAULT_MAX_BYTES,
            extras_after_payload: false,
            accept_untagged: false,
            _doc: PhantomData,
        }
    }

    /// The migration chain (`[v1→v2, v2→v3, …]`).
    pub fn migrations(mut self, m: Vec<Migration>) -> Self {
        self.migrations = m;
        self
    }

    /// The envelope key holding the document (default `"document"`).
    pub fn payload_key(mut self, key: impl Into<String>) -> Self {
        self.payload_key = key.into();
        self
    }

    /// Recognise bare pre-envelope documents.
    pub fn legacy(mut self, probe: LegacyProbe) -> Self {
        self.legacy = Some(probe);
        self
    }

    /// Refuse files older than `v` ([`FormatError::TooOld`]).
    pub fn oldest(mut self, v: u64) -> Self {
        self.oldest = v.max(1);
        self
    }

    /// Indented (diffable, the default) or compact (autosaves) output.
    pub fn pretty(mut self, on: bool) -> Self {
        self.pretty = on;
        self
    }

    /// Refuse files larger than this.
    pub fn max_bytes(mut self, n: u64) -> Self {
        self.max_bytes = n;
        self
    }

    /// Write extra envelope keys after the payload key instead of before it (default `false`).
    /// Reading accepts either order.
    pub fn extras_after_payload(mut self, on: bool) -> Self {
        self.extras_after_payload = on;
        self
    }

    /// Read envelopes that have no `format` tag (early files) when they have a `schema_version`
    /// and the payload key; such files are reported with this format's id. Off by default:
    /// they are refused as [`FormatError::NotThisFormat`]. A `format` tag naming another format is
    /// always refused.
    pub fn accept_untagged(mut self, on: bool) -> Self {
        self.accept_untagged = on;
        self
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    /// The schema this build writes and the newest it reads.
    pub fn schema_version(&self) -> u64 {
        self.migrations.len() as u64 + 1
    }

    // ------------------------------------------------------------------ writing

    /// The file bytes for `doc` in the current schema.
    pub fn encode(&self, doc: &D) -> Result<Vec<u8>, FormatError> {
        self.encode_with(doc, &Map::new())
    }

    /// [`Format::encode`] with extra envelope keys (envelope fields and the payload key win).
    pub fn encode_with(&self, doc: &D, extras: &Map<String, Value>) -> Result<Vec<u8>, FormatError> {
        let out = EnvelopeOut {
            format: &self.id,
            schema_version: self.schema_version(),
            generator: &self.generator,
            key: &self.payload_key,
            doc,
            extras,
            extras_after: self.extras_after_payload,
        };
        let r = if self.pretty { serde_json::to_vec_pretty(&out) } else { serde_json::to_vec(&out) };
        r.map_err(|e| FormatError::Encode(e.to_string()))
    }

    /// Encode and write `doc` to `path` durably.
    pub fn save(&self, path: &Path, doc: &D) -> Result<(), FormatError> {
        let bytes = self.encode(doc)?;
        ashlar_fs::atomic_write(path, &bytes)?;
        Ok(())
    }

    // ------------------------------------------------------------------ reading

    /// Read a file from disk.
    pub fn load(&self, path: &Path) -> Result<Loaded<D>, FormatError> {
        let len = std::fs::metadata(path)?.len();
        if len > self.max_bytes {
            return Err(FormatError::Corrupt(format!("file is {len} bytes, more than the {} allowed", self.max_bytes)));
        }
        let bytes = std::fs::read(path)?;
        self.decode(&bytes)
    }

    /// Read file bytes (any supported schema).
    pub fn decode(&self, bytes: &[u8]) -> Result<Loaded<D>, FormatError> {
        if bytes.len() as u64 > self.max_bytes {
            return Err(FormatError::Corrupt(format!("{} bytes, more than the {} allowed", bytes.len(), self.max_bytes)));
        }
        let mut de = serde_json::Deserializer::from_slice(bytes);
        let probe = ProbeSeed { key: &self.payload_key }.deserialize(&mut de).and_then(|p| de.end().map(|()| p)).map_err(|e| self.parse_error(e))?;
        match probe.schema_version {
            Some(v) => {
                let found = version_of(&v)?;
                match probe.format {
                    Some(f) if f == self.id => {}
                    Some(f) => return Err(FormatError::WrongFormat { found: f, expected: self.id.clone() }),
                    None if self.accept_untagged && probe.has_payload => {}
                    None => return Err(self.not_this("no `format` tag")),
                }
                self.check_supported(found)?;
                if found == self.schema_version() {
                    // Fast path: one pass straight into `D`.
                    let mut de = serde_json::Deserializer::from_slice(bytes);
                    let (doc, extras, generator) = EnvelopeSeed::<D> { key: &self.payload_key, _d: PhantomData }
                        .deserialize(&mut de)
                        .map_err(|e| FormatError::Corrupt(e.to_string()))?;
                    let doc = doc.ok_or_else(|| FormatError::Corrupt(format!("missing `{}`", self.payload_key)))?;
                    let envelope = Envelope { format: self.id.clone(), schema_version: found, generator };
                    return Ok(Loaded { doc, envelope, extras });
                }
                let value: Value = serde_json::from_slice(bytes).map_err(|e| self.parse_error(e))?;
                self.upgrade(value, found)
            }
            None => {
                let value: Value = serde_json::from_slice(bytes).map_err(|e| self.parse_error(e))?;
                let found = self.legacy.and_then(|p| p(&value)).ok_or_else(|| self.not_this("no `schema_version`"))?;
                self.check_supported(found)?;
                self.upgrade(value, found)
            }
        }
    }

    /// Run the migrations from `found` and read the result.
    fn upgrade(&self, value: Value, found: u64) -> Result<Loaded<D>, FormatError> {
        let generator = value.get(GENERATOR).and_then(Value::as_str).map(str::to_string);
        let format = value.get(FORMAT).and_then(Value::as_str).unwrap_or(&self.id).to_string();
        let envelope = Envelope { format, schema_version: found, generator };
        let mut out = migrate(&self.migrations, value, found)?;
        let Some(obj) = out.as_object_mut() else { return Err(FormatError::Corrupt("not a JSON object".into())) };
        let payload = obj
            .remove(&self.payload_key)
            .ok_or_else(|| FormatError::Corrupt(format!("after upgrading from schema v{found}: missing `{}`", self.payload_key)))?;
        let doc = D::deserialize(payload).map_err(|e| FormatError::Corrupt(format!("after upgrading from schema v{found}: {e}")))?;
        for k in [FORMAT, SCHEMA, GENERATOR] {
            obj.remove(k);
        }
        Ok(Loaded { doc, envelope, extras: std::mem::take(obj) })
    }

    fn check_supported(&self, found: u64) -> Result<(), FormatError> {
        if found > self.schema_version() {
            return Err(FormatError::TooNew { found, supported: self.schema_version() });
        }
        if found < self.oldest {
            return Err(FormatError::TooOld { found, oldest: self.oldest });
        }
        Ok(())
    }

    fn not_this(&self, why: &str) -> FormatError {
        FormatError::NotThisFormat { expected: self.id.clone(), why: why.into(), detail: None }
    }

    fn parse_error(&self, e: serde_json::Error) -> FormatError {
        if e.is_syntax() || e.is_eof() {
            FormatError::Corrupt(e.to_string())
        } else {
            FormatError::NotThisFormat { expected: self.id.clone(), why: e.to_string(), detail: Some(JsonError::new(&e)) }
        }
    }
}

/// Run `table` on `doc` from schema `from` up to `table.len() + 1`, stamping `schema_version`
/// after each step.
pub fn migrate(table: &[Migration], mut doc: Value, from: u64) -> Result<Value, FormatError> {
    let target = table.len() as u64 + 1;
    if from > target {
        return Err(FormatError::TooNew { found: from, supported: target });
    }
    if from == 0 {
        return Err(FormatError::TooOld { found: 0, oldest: 1 });
    }
    for v in from..target {
        let step = usize::try_from(v - 1).ok().and_then(|i| table.get(i)).ok_or(FormatError::Migration { from: v, msg: "no migration".into() })?;
        doc = step(doc).map_err(|msg| FormatError::Migration { from: v, msg })?;
        match doc.as_object_mut() {
            Some(o) => {
                o.insert(SCHEMA.into(), Value::from(v + 1));
            }
            None => return Err(FormatError::Migration { from: v, msg: "migration produced a non-object document".into() }),
        }
    }
    Ok(doc)
}

/// A schema version as found in a file: a non-negative integer. Integers too large for `u64`
/// (written as floats by JSON) are from the far future.
fn version_of(v: &Value) -> Result<u64, FormatError> {
    if let Some(n) = v.as_u64() {
        return Ok(n);
    }
    match v.as_f64() {
        Some(f) if f >= u64::MAX as f64 && f.fract() == 0.0 => Ok(u64::MAX),
        _ => Err(FormatError::Corrupt(format!("`schema_version` is not a version number: {v}"))),
    }
}

/// Just enough of a file to know what it is.
#[derive(Default)]
struct Probe {
    format: Option<String>,
    schema_version: Option<Value>,
    has_payload: bool,
}

/// Reads a [`Probe`], skipping everything else (the payload included) without building it.
struct ProbeSeed<'a> {
    key: &'a str,
}

impl<'de> DeserializeSeed<'de> for ProbeSeed<'_> {
    type Value = Probe;
    fn deserialize<De: Deserializer<'de>>(self, de: De) -> Result<Probe, De::Error> {
        de.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for ProbeSeed<'_> {
    type Value = Probe;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a document envelope object")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Probe, A::Error> {
        let mut p = Probe::default();
        let (mut seen_format, mut seen_schema) = (false, false);
        while let Some(k) = map.next_key::<String>()? {
            if k == FORMAT {
                if std::mem::replace(&mut seen_format, true) {
                    return Err(serde::de::Error::duplicate_field(FORMAT));
                }
                p.format = map.next_value()?;
            } else if k == SCHEMA {
                if std::mem::replace(&mut seen_schema, true) {
                    return Err(serde::de::Error::duplicate_field(SCHEMA));
                }
                p.schema_version = map.next_value()?;
            } else {
                p.has_payload |= k == self.key;
                map.next_value::<IgnoredAny>()?;
            }
        }
        Ok(p)
    }
    /// A bare array is no envelope, but may be a pre-envelope document: left to the legacy probe.
    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Probe, A::Error> {
        while seq.next_element::<IgnoredAny>()?.is_some() {}
        Ok(Probe::default())
    }
}

struct EnvelopeOut<'a, D> {
    format: &'a str,
    schema_version: u64,
    generator: &'a str,
    key: &'a str,
    doc: &'a D,
    extras: &'a Map<String, Value>,
    extras_after: bool,
}

impl<D: Serialize> Serialize for EnvelopeOut<'_, D> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let reserved = [FORMAT, SCHEMA, GENERATOR, self.key];
        let mut m = s.serialize_map(None)?;
        m.serialize_entry(FORMAT, self.format)?;
        m.serialize_entry(SCHEMA, &self.schema_version)?;
        m.serialize_entry(GENERATOR, self.generator)?;
        let extras = self.extras.iter().filter(|(k, _)| !reserved.contains(&k.as_str()));
        if self.extras_after {
            m.serialize_entry(self.key, self.doc)?;
            for (k, v) in extras {
                m.serialize_entry(k, v)?;
            }
        } else {
            for (k, v) in extras {
                m.serialize_entry(k, v)?;
            }
            m.serialize_entry(self.key, self.doc)?;
        }
        m.end()
    }
}

/// Reads a current-schema envelope in one pass: the payload straight into `D`, other keys kept.
struct EnvelopeSeed<'a, D> {
    key: &'a str,
    _d: PhantomData<fn() -> D>,
}

type Parts<D> = (Option<D>, Map<String, Value>, Option<String>);

impl<'de, D: DeserializeOwned> DeserializeSeed<'de> for EnvelopeSeed<'_, D> {
    type Value = Parts<D>;
    fn deserialize<De: Deserializer<'de>>(self, de: De) -> Result<Parts<D>, De::Error> {
        de.deserialize_map(self)
    }
}

impl<'de, D: DeserializeOwned> Visitor<'de> for EnvelopeSeed<'_, D> {
    type Value = Parts<D>;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a document envelope object")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Parts<D>, A::Error> {
        let (mut doc, mut extras, mut generator) = (None, Map::new(), None);
        while let Some(k) = map.next_key::<String>()? {
            if k == self.key {
                if doc.is_some() {
                    return Err(serde::de::Error::custom(format!("duplicate `{k}`")));
                }
                doc = Some(map.next_value::<D>()?);
            } else if k == GENERATOR {
                generator = map.next_value::<Option<String>>()?;
            } else if k == FORMAT || k == SCHEMA {
                map.next_value::<IgnoredAny>()?;
            } else {
                extras.insert(k, map.next_value::<Value>()?);
            }
        }
        Ok((doc, extras, generator))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Doc {
        title: String,
        #[serde(default)]
        tags: Vec<String>,
    }

    fn doc() -> Doc {
        Doc { title: "hello".into(), tags: vec!["a".into()] }
    }

    fn v1_to_v2(v: Value) -> Result<Value, String> {
        // v1 was the bare document with `name` instead of `title`.
        let Value::Object(mut o) = v else { return Err("expected an object".into()) };
        let name = o.remove("name").unwrap_or(Value::from(""));
        o.insert("title".into(), name);
        Ok(json!({ "format": "test.doc", "schema_version": 2, "generator": "v1", "document": o }))
    }
    fn v2_to_v3(v: Value) -> Result<Value, String> {
        Ok(v)
    }
    fn failing(_: Value) -> Result<Value, String> {
        Err("cannot".into())
    }
    fn not_object(_: Value) -> Result<Value, String> {
        Ok(Value::from(3))
    }

    fn fmt() -> Format<Doc> {
        Format::new("test.doc", "Test 1.0").migrations(vec![v1_to_v2, v2_to_v3]).legacy(|v| v.get("name").map(|_| 1))
    }

    #[test]
    fn round_trip_current_schema() {
        let f = fmt();
        assert_eq!(f.schema_version(), 3);
        let bytes = f.encode(&doc()).unwrap();
        let text = String::from_utf8(bytes.clone()).unwrap();
        assert!(text.starts_with("{\n  \"format\": \"test.doc\""), "{text}");
        let l = f.decode(&bytes).unwrap();
        assert_eq!(l.doc, doc());
        assert_eq!(l.envelope, Envelope { format: "test.doc".into(), schema_version: 3, generator: Some("Test 1.0".into()) });
        assert!(!l.migrated(f.schema_version()));
        assert!(l.extras.is_empty());
    }

    #[test]
    fn extras_round_trip_and_cannot_clobber_reserved_keys() {
        let f = fmt().pretty(false);
        let mut extras = Map::new();
        extras.insert("view".into(), json!({"zoom": 2}));
        extras.insert("schema_version".into(), json!(99));
        extras.insert("document".into(), json!("evil"));
        let bytes = f.encode_with(&doc(), &extras).unwrap();
        let l = f.decode(&bytes).unwrap();
        assert_eq!(l.doc, doc());
        assert_eq!(l.envelope.schema_version, 3);
        assert_eq!(l.extras.len(), 1);
        assert_eq!(l.extras["view"], json!({"zoom": 2}));
    }

    #[test]
    fn extras_after_payload_reencodes_byte_for_byte() {
        // A file written by hand-rolled code: payload first, then extras.
        #[derive(Serialize)]
        struct Written<'a> {
            format: &'a str,
            schema_version: u64,
            generator: &'a str,
            document: &'a Doc,
            view: Value,
        }
        let written = Written { format: "test.doc", schema_version: 3, generator: "Test 1.0", document: &doc(), view: json!({"zoom": 2}) };
        for pretty in [true, false] {
            let file = if pretty { serde_json::to_vec_pretty(&written) } else { serde_json::to_vec(&written) }.unwrap();
            let f = fmt().pretty(pretty).extras_after_payload(true);
            let l = f.decode(&file).unwrap();
            assert_eq!(l.extras["view"], json!({"zoom": 2}));
            assert_eq!(f.encode_with(&l.doc, &l.extras).unwrap(), file, "pretty: {pretty}");
            // The default still writes extras first, and both orders read back.
            let before = fmt().pretty(pretty).encode_with(&l.doc, &l.extras).unwrap();
            assert_ne!(before, file);
            assert_eq!(f.decode(&before).unwrap().extras, l.extras);
        }
        // Reserved keys still can't be clobbered from the extras.
        let mut extras = Map::new();
        extras.insert("document".into(), json!("evil"));
        extras.insert("format".into(), json!("other"));
        let f = fmt().extras_after_payload(true);
        let l = f.decode(&f.encode_with(&doc(), &extras).unwrap()).unwrap();
        assert_eq!(l.doc, doc());
        assert!(l.extras.is_empty());
    }

    #[test]
    fn untagged_envelopes_only_when_accepted() {
        let current = br#"{"schema_version":3,"generator":"Early","document":{"title":"t"},"view":1}"#;
        let older = br#"{"schema_version":2,"document":{"title":"o"}}"#;
        let refuse = fmt();
        for b in [&current[..], &older[..]] {
            let e = refuse.decode(b).unwrap_err();
            assert!(matches!(&e, FormatError::NotThisFormat { why, detail: None, .. } if why.contains("format")), "{e}");
        }
        let accept = fmt().accept_untagged(true);
        let l = accept.decode(current).unwrap();
        assert_eq!(l.doc.title, "t");
        assert_eq!(l.envelope, Envelope { format: "test.doc".into(), schema_version: 3, generator: Some("Early".into()) });
        assert_eq!(l.extras["view"], json!(1));
        let l = accept.decode(older).unwrap();
        assert_eq!(l.doc.title, "o");
        assert_eq!(l.envelope.format, "test.doc");
        assert!(l.migrated(accept.schema_version()));
        // Not this format: no payload key, another format's tag, or a newer schema.
        let e = accept.decode(br#"{"schema_version":3,"project":{}}"#).unwrap_err();
        assert!(matches!(e, FormatError::NotThisFormat { .. }), "{e}");
        let e = accept.decode(br#"{"format":"other.doc","schema_version":3,"document":{}}"#).unwrap_err();
        assert!(matches!(e, FormatError::WrongFormat { .. }), "{e}");
        let e = accept.decode(br#"{"schema_version":4,"document":{}}"#).unwrap_err();
        assert!(matches!(e, FormatError::TooNew { found: 4, .. }), "{e}");
        let e = accept.decode(br#"{"format":null,"schema_version":3,"document":{"title":"n"}}"#);
        assert_eq!(e.unwrap().doc.title, "n", "a null tag is no tag");
    }

    #[test]
    fn errors_keep_the_serde_error() {
        let f = fmt();
        // Valid JSON of the wrong shape: refused, with serde's message and position.
        let bad = b"{\n  \"format\": 7,\n  \"schema_version\": 3\n}";
        let e = f.decode(bad).unwrap_err();
        let j = e.json().cloned().unwrap();
        assert_eq!((j.line, j.column), (2, 13), "{j:?}");
        assert!(!j.syntax);
        assert!(j.message.contains("invalid type: integer `7`") && j.message.ends_with("at line 2 column 13"), "{}", j.message);
        assert!(matches!(&e, FormatError::NotThisFormat { why, .. } if *why == j.message));
        let e = f.decode(b"[1, 2]").unwrap_err();
        assert!(e.json().is_none(), "not legacy, not a parse error: {e}");
        let e = f.decode(br#"{"format":"test.doc","format":"test.doc","schema_version":3}"#).unwrap_err();
        assert!(e.json().is_some_and(|j| j.message.contains("duplicate field `format`")), "{e}");
        // Malformed JSON: `Corrupt` with serde's message verbatim.
        let broken = b"{\n  \"format\": \"test.doc\",\n  \"schema_version\": 3,,\n}";
        let expected = serde_json::from_slice::<Value>(broken).unwrap_err().to_string();
        let e = f.decode(broken).unwrap_err();
        assert!(matches!(&e, FormatError::Corrupt(m) if *m == expected), "{e}");
        assert!(expected.ends_with("at line 3 column 23"), "{expected}");
        assert!(e.json().is_none());
        let e = f.decode(br#"{"format":"test.doc","schema_version":3,"document":{}} trailing"#).unwrap_err();
        assert!(matches!(e, FormatError::Corrupt(_)), "{e}");
    }

    #[test]
    fn migrates_legacy_and_older_envelopes() {
        let f = fmt();
        let l = f.decode(br#"{"name":"old","tags":["x"]}"#).unwrap();
        assert_eq!(l.doc, Doc { title: "old".into(), tags: vec!["x".into()] });
        assert_eq!(l.envelope.schema_version, 1);
        assert!(l.migrated(f.schema_version()));
        let l = f.decode(br#"{"format":"test.doc","schema_version":2,"generator":"Old","document":{"title":"t"},"view":1}"#).unwrap();
        assert_eq!(l.doc.title, "t");
        assert_eq!(l.envelope.generator.as_deref(), Some("Old"));
        assert_eq!(l.extras["view"], json!(1));
    }

    #[test]
    fn refuses_newer_and_other_formats() {
        let f = fmt();
        let e = f.decode(br#"{"format":"test.doc","schema_version":4,"document":{}}"#).unwrap_err();
        assert!(matches!(e, FormatError::TooNew { found: 4, supported: 3 }), "{e}");
        let e = f.decode(br#"{"format":"test.doc","schema_version":18446744073709551615,"document":{}}"#).unwrap_err();
        assert!(matches!(e, FormatError::TooNew { found: u64::MAX, .. }), "{e}");
        let e = f.decode(br#"{"format":"test.doc","schema_version":1e300,"document":{}}"#).unwrap_err();
        assert!(matches!(e, FormatError::TooNew { found: u64::MAX, .. }), "{e}");
        let e = f.decode(br#"{"format":"test.doc","schema_version":999999999999999999999999999,"document":{}}"#).unwrap_err();
        assert!(matches!(e, FormatError::TooNew { .. }), "{e}");
        let e = f.decode(br#"{"format":"other.doc","schema_version":3,"document":{}}"#).unwrap_err();
        assert!(matches!(e, FormatError::WrongFormat { .. }), "{e}");
        let e = f.decode(br#"{"schema_version":3,"document":{}}"#).unwrap_err();
        assert!(matches!(e, FormatError::NotThisFormat { .. }), "{e}");
        let e = f.decode(br#"{"title":"no envelope, not legacy"}"#).unwrap_err();
        assert!(matches!(e, FormatError::NotThisFormat { .. }), "{e}");
        let e = Format::<Doc>::new("test.doc", "t").oldest(2).decode(br#"{"format":"test.doc","schema_version":1,"document":{}}"#).unwrap_err();
        assert!(matches!(e, FormatError::TooOld { found: 1, oldest: 2 }), "{e}");
    }

    #[test]
    fn hostile_inputs_are_errors_not_crashes() {
        let f = fmt();
        let cases: &[&[u8]] = &[
            b"",
            b"{",
            b"\xff\xfe",
            b"[]",
            b"null",
            b"42",
            br#"{"format":"test.doc","schema_version":-1,"document":{}}"#,
            br#"{"format":"test.doc","schema_version":0,"document":{}}"#,
            br#"{"format":"test.doc","schema_version":2.5,"document":{}}"#,
            br#"{"format":"test.doc","schema_version":"3","document":{}}"#,
            br#"{"format":"test.doc","schema_version":3}"#,
            br#"{"format":"test.doc","schema_version":3,"document":{"title":5}}"#,
            br#"{"format":"test.doc","schema_version":3,"document":{"title":"a"},"document":{"title":"b"}}"#,
            br#"{"format":"test.doc","schema_version":2,"document":[]}"#,
            br#"{"format":7,"schema_version":3,"document":{}}"#,
            br#"{"format":"test.doc","schema_version":3,"generator":5,"document":{"title":"a"}}"#,
        ];
        for c in cases {
            assert!(f.decode(c).is_err(), "{}", String::from_utf8_lossy(c));
        }
        // Truncations of a valid file at every length.
        let good = f.encode(&doc()).unwrap();
        for n in 0..good.len() {
            assert!(f.decode(&good[..n]).is_err(), "truncated at {n}");
        }
        // Deep nesting hits the parser's recursion limit, not the stack.
        let deep =
            format!(r#"{{"format":"test.doc","schema_version":3,"document":{{"title":"x","tags":{}{}}}}}"#, "[".repeat(100_000), "]".repeat(100_000));
        assert!(f.decode(deep.as_bytes()).is_err());
        let small = Format::<Doc>::new("test.doc", "t").max_bytes(10);
        assert!(matches!(small.decode(&good), Err(FormatError::Corrupt(_))));
    }

    #[test]
    fn migration_errors() {
        let f = Format::<Doc>::new("test.doc", "t").migrations(vec![failing]);
        let e = f.decode(br#"{"format":"test.doc","schema_version":1,"document":{"title":"a"}}"#).unwrap_err();
        assert!(matches!(e, FormatError::Migration { from: 1, .. }), "{e}");
        let f = Format::<Doc>::new("test.doc", "t").migrations(vec![not_object]);
        let e = f.decode(br#"{"format":"test.doc","schema_version":1,"document":{"title":"a"}}"#).unwrap_err();
        assert!(matches!(e, FormatError::Migration { from: 1, .. }), "{e}");
        assert!(matches!(migrate(&[], json!({}), 0), Err(FormatError::TooOld { .. })));
        assert!(matches!(migrate(&[], json!({}), 5), Err(FormatError::TooNew { .. })));
        assert_eq!(migrate(&[v2_to_v3], json!({}), 1).unwrap()["schema_version"], 2);
    }

    #[test]
    fn save_and_load_files() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let p = dir.join("a.testdoc");
        let f = fmt();
        f.save(&p, &doc()).unwrap();
        assert_eq!(f.load(&p).unwrap().doc, doc());
        assert!(matches!(f.load(&dir.join("missing")), Err(FormatError::Io(_))));
        assert!(matches!(fmt().max_bytes(5).load(&p), Err(FormatError::Corrupt(_))));
    }
}
