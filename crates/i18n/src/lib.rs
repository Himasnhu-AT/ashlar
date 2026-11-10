//! Translations for ashlar apps: catalogs per locale, `{name}` templates, CLDR plural categories,
//! number formatting and locale negotiation. Small on purpose: a plural-rules table for common
//! languages instead of a full Unicode library, and it builds for wasm32.
//!
//! ```
//! use ashlar_i18n::I18n;
//!
//! let mut i18n = I18n::new("en");
//! i18n.add("en", r#"{"files.one": "{count} file", "files.other": "{count} files", "hi": "Hello, {name}"}"#);
//! i18n.add("de", r#"{"files.one": "{count} Datei", "files.other": "{count} Dateien"}"#);
//! i18n.set_locale(&i18n.negotiate(&["de-AT", "en"]));
//! assert_eq!(i18n.plural("files", 1200, &[]), "1.200 Dateien");
//! assert_eq!(i18n.format("hi", &[("name", "Ada")]), "Hello, Ada"); // falls back to English
//! ```
//!
//! Catalogs are flat JSON objects (`{"key": "text"}`; nested objects are flattened with dots),
//! usually embedded with `include_str!` and parsed on first use. A bad catalog is reported by
//! [`I18n::problems`] and logged, and acts as empty: lookups fall back, nothing panics.
//! Plural forms are keys with a category suffix (`key.one`, `key.few`, `key.other`).
//! Lookups go down the fallback chain locale → language → source locale; a key missing
//! everywhere shows as itself.
//!
//! [`I18n::check`] compares every catalog with the source locale's keys and placeholders; run it
//! in a test so a missing translation fails CI.

mod catalog;
pub mod locale;
pub mod number;
pub mod plural;

use std::borrow::Cow;
use std::fmt;
use std::sync::OnceLock;

pub use catalog::{CatalogError, MAX_CATALOG_BYTES, fill, placeholders};
pub use locale::{Locale, negotiate, system_locales};
pub use number::Symbols;
pub use plural::{Category, Operands};

const CATEGORIES: [Category; 6] = [Category::Zero, Category::One, Category::Two, Category::Few, Category::Many, Category::Other];

struct Layer {
    locale: Locale,
    json: Cow<'static, str>,
    parsed: OnceLock<(catalog::Entries, Vec<CatalogError>)>,
}

impl Layer {
    fn entries(&self) -> &catalog::Entries {
        &self.parsed().0
    }

    fn parsed(&self) -> &(catalog::Entries, Vec<CatalogError>) {
        self.parsed.get_or_init(|| {
            let mut problems = Vec::new();
            let entries = catalog::parse(self.locale.as_str(), &self.json, &mut problems);
            for p in &problems {
                log::warn!("ashlar-i18n: {p}");
            }
            (entries, problems)
        })
    }
}

impl Clone for Layer {
    fn clone(&self) -> Self {
        Layer { locale: self.locale.clone(), json: self.json.clone(), parsed: self.parsed.clone() }
    }
}

/// Something [`I18n::check`] found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Problem {
    /// A catalog could not be read (fully or partly).
    Catalog(CatalogError),
    /// A catalog was added under a tag that is not a locale.
    BadLocale(String),
    /// `locale` has no text for a key the source locale has.
    MissingKey { locale: Locale, key: String },
    /// `locale`'s text uses different `{placeholders}` than the source's.
    Placeholders { locale: Locale, key: String },
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Problem::Catalog(e) => write!(f, "{e}"),
            Problem::BadLocale(tag) => write!(f, "{tag:?} is not a locale tag"),
            Problem::MissingKey { locale, key } => write!(f, "{locale}: missing {key}"),
            Problem::Placeholders { locale, key } => write!(f, "{locale}: {key} has different placeholders than the source"),
        }
    }
}

/// Catalogs plus the current locale.
#[derive(Clone)]
pub struct I18n {
    source: Locale,
    locale: Locale,
    /// Locales to look in, in order (current locale's fallbacks, then the source's).
    chain: Vec<Locale>,
    symbols: Symbols,
    layers: Vec<Layer>,
    bad_tags: Vec<String>,
}

impl fmt::Debug for I18n {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("I18n").field("source", &self.source).field("locale", &self.locale).field("layers", &self.layers.len()).finish()
    }
}

impl Default for I18n {
    fn default() -> Self {
        I18n::new("en")
    }
}

impl I18n {
    /// No catalogs yet; `source` is the locale every key exists in (English when the tag is not
    /// valid). The current locale starts as the source.
    pub fn new(source: &str) -> Self {
        let source = Locale::parse(source).unwrap_or_else(Locale::english);
        let mut i18n = I18n {
            locale: source.clone(),
            chain: Vec::new(),
            symbols: Symbols::for_locale(source.as_str()),
            source,
            layers: Vec::new(),
            bad_tags: Vec::new(),
        };
        i18n.rebuild_chain();
        i18n
    }

    /// Adds a catalog for `locale`, parsed on first use. Later catalogs for the same locale win
    /// key by key, so an app layers its own texts (or overrides) over a library's.
    pub fn add(&mut self, locale: &str, json: impl Into<Cow<'static, str>>) -> &mut Self {
        match Locale::parse(locale) {
            Some(locale) => self.layers.push(Layer { locale, json: json.into(), parsed: OnceLock::new() }),
            None => {
                log::warn!("ashlar-i18n: catalog tag {locale:?} is not a locale; ignored");
                self.bad_tags.push(locale.chars().take(64).collect());
            }
        }
        self
    }

    /// [`add`](Self::add) for a builder chain.
    pub fn with(mut self, locale: &str, json: impl Into<Cow<'static, str>>) -> Self {
        self.add(locale, json);
        self
    }

    /// The source locale.
    pub fn source(&self) -> &Locale {
        &self.source
    }

    /// The current locale.
    pub fn locale(&self) -> &Locale {
        &self.locale
    }

    /// Switches the current locale (any tag; lookups fall back for what it lacks). Returns
    /// `false` and changes nothing when `tag` is not a locale.
    pub fn set_locale(&mut self, tag: &str) -> bool {
        let Some(locale) = Locale::parse(tag) else { return false };
        self.symbols = Symbols::for_locale(locale.as_str());
        self.locale = locale;
        self.rebuild_chain();
        true
    }

    /// The locales that have a catalog, plus the source, sorted.
    pub fn available(&self) -> Vec<Locale> {
        let mut out: Vec<Locale> = self.layers.iter().map(|l| l.locale.clone()).collect();
        out.push(self.source.clone());
        out.sort();
        out.dedup();
        out
    }

    /// The best available locale for a user who prefers `preferred` (most preferred first, any
    /// tag syntax, e.g. [`system_locales`]); the source locale when nothing matches.
    pub fn negotiate<S: AsRef<str>>(&self, preferred: &[S]) -> String {
        let preferred: Vec<Locale> = preferred.iter().filter_map(|p| Locale::parse(p.as_ref())).collect();
        let available = self.available();
        locale::negotiate(&preferred, &available).unwrap_or(&self.source).to_string()
    }

    /// The text for `key` in the current locale, falling back along the chain; the key itself
    /// when no catalog has it.
    pub fn text<'a>(&'a self, key: &'a str) -> &'a str {
        self.get(key).unwrap_or(key)
    }

    /// The text for `key`, or `None` when no catalog in the chain has it.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.chain.iter().find_map(|l| self.lookup(l, key))
    }

    /// [`text`](Self::text) with `{name}` placeholders filled from `args`.
    pub fn format(&self, key: &str, args: &[(&str, &str)]) -> String {
        fill(self.text(key), args)
    }

    /// The plural form of `key` for `count` (`key.one`, `key.other`, … by the current locale's
    /// rules), with `{count}` filled in as a localized integer unless `args` sets it.
    pub fn plural(&self, key: &str, count: i64, args: &[(&str, &str)]) -> String {
        let shown = self.integer(count);
        let mut all: Vec<(&str, &str)> = Vec::with_capacity(args.len() + 1);
        all.extend_from_slice(args);
        if !args.iter().any(|(n, _)| *n == "count") {
            all.push(("count", &shown));
        }
        let mut suffixed = String::with_capacity(key.len() + 6);
        for locale in &self.chain {
            // each locale's own rules: a fallback language has its own categories
            let category = plural::category(locale.as_str(), Operands::int(count));
            for c in [category, Category::Other] {
                suffixed.clear();
                suffixed.push_str(key);
                suffixed.push('.');
                suffixed.push_str(c.as_str());
                if let Some(t) = self.lookup(locale, &suffixed) {
                    return fill(t, &all);
                }
            }
        }
        fill(self.text(key), &all)
    }

    /// The plural category of `count` in the current locale.
    pub fn category(&self, count: i64) -> Category {
        plural::category(self.locale.as_str(), Operands::int(count))
    }

    /// `n` with the current locale's group separators (`1,234` / `1.234`).
    pub fn integer(&self, n: i64) -> String {
        number::integer(&self.symbols, n)
    }

    /// `value` rounded to `fraction_digits` with the current locale's separators.
    pub fn decimal(&self, value: f64, fraction_digits: u8) -> String {
        number::decimal(&self.symbols, value, fraction_digits)
    }

    /// Catalog parse problems and bad tags (parses every catalog).
    pub fn problems(&self) -> Vec<Problem> {
        let mut out: Vec<Problem> = self.bad_tags.iter().cloned().map(Problem::BadLocale).collect();
        out.extend(self.layers.iter().flat_map(|l| l.parsed().1.iter().cloned().map(Problem::Catalog)));
        out
    }

    /// Every [`problem`](Self::problems), plus each key the source locale has that another
    /// locale lacks (for plural keys, `key.other`), and texts whose placeholders differ from the
    /// source's. Empty when the catalogs are complete. Regional catalogs (`de-AT`) only add to
    /// their language's and are not checked for completeness.
    pub fn check(&self) -> Vec<Problem> {
        let mut out = self.problems();
        let source_keys = self.keys(&self.source);
        for locale in self.available() {
            if locale == self.source || locale.region().is_some() {
                continue;
            }
            let mut missing: Vec<String> = Vec::new();
            for key in &source_keys {
                let (base, plural) = split_plural(key);
                if plural {
                    let other = format!("{base}.other");
                    if self.lookup(&locale, &other).is_none() && !missing.contains(&other) {
                        missing.push(other.clone());
                    }
                    // a form may leave out `{count}` ("one file"), but never invent a placeholder
                    if let (Some(src), Some(t)) =
                        (self.lookup(&self.source, &other).or_else(|| self.lookup(&self.source, key)), self.lookup(&locale, key))
                        && !placeholders(t).iter().all(|p| placeholders(src).contains(p))
                    {
                        out.push(Problem::Placeholders { locale: locale.clone(), key: key.clone() });
                    }
                    continue;
                }
                match self.lookup(&locale, key) {
                    None => missing.push(key.clone()),
                    Some(t) => {
                        if let Some(src) = self.lookup(&self.source, key)
                            && !same_placeholders(src, t)
                        {
                            out.push(Problem::Placeholders { locale: locale.clone(), key: key.clone() });
                        }
                    }
                }
            }
            out.extend(missing.into_iter().map(|key| Problem::MissingKey { locale: locale.clone(), key }));
        }
        out
    }

    /// The keys among `keys` that the source locale has no text for (plural keys count when
    /// `key.other` exists). Pair it with a `const` key table to catch typos in a test.
    pub fn missing_source_keys<'k>(&self, keys: &[&'k str]) -> Vec<&'k str> {
        keys.iter()
            .copied()
            .filter(|k| self.lookup(&self.source, k).is_none() && self.lookup(&self.source, &format!("{k}.other")).is_none())
            .collect()
    }

    fn keys(&self, locale: &Locale) -> Vec<String> {
        let mut keys: Vec<String> =
            self.layers.iter().filter(|l| l.locale == *locale).flat_map(|l| l.entries().map.keys().map(|k| k.to_string())).collect();
        keys.sort_unstable();
        keys.dedup();
        keys
    }

    fn lookup(&self, locale: &Locale, key: &str) -> Option<&str> {
        self.layers.iter().rev().filter(|l| l.locale == *locale).find_map(|l| l.entries().map.get(key).map(|s| &**s))
    }

    fn rebuild_chain(&mut self) {
        let mut chain = self.locale.fallbacks();
        for l in self.source.fallbacks() {
            if !chain.contains(&l) {
                chain.push(l);
            }
        }
        self.chain = chain;
    }
}

/// `("files", true)` for `files.one`.
fn split_plural(key: &str) -> (&str, bool) {
    match key.rsplit_once('.') {
        Some((base, suffix)) if CATEGORIES.iter().any(|c| c.as_str() == suffix) => (base, true),
        _ => (key, false),
    }
}

fn same_placeholders(a: &str, b: &str) -> bool {
    placeholders(a) == placeholders(b)
}

impl AsRef<str> for Locale {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}
