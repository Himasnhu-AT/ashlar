#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ashlar_i18n::{I18n, Problem};

const EN: &str = r#"{
  "app.title": "Notes",
  "greet": "Hello, {name}!",
  "files.one": "{count} file",
  "files.other": "{count} files",
  "only.en": "English only"
}"#;
const DE: &str = r#"{
  "app.title": "Notizen",
  "greet": "Hallo, {name}!",
  "files.one": "eine Datei",
  "files.other": "{count} Dateien",
  "only.en": "Nur Englisch"
}"#;
const RU: &str = r#"{"files.one": "{count} файл", "files.few": "{count} файла", "files.many": "{count} файлов", "files.other": "{count} файла"}"#;

fn i18n() -> I18n {
    I18n::new("en").with("en", EN).with("de", DE).with("ru", RU).with("de-AT", r#"{"app.title": "Notizen (AT)"}"#)
}

#[test]
fn fallback_chain_locale_language_source() {
    let mut i = i18n();
    assert!(i.set_locale("de-AT"));
    assert_eq!(i.text("app.title"), "Notizen (AT)");
    assert_eq!(i.text("greet"), "Hallo, {name}!");
    i.set_locale("de-CH");
    assert_eq!(i.text("app.title"), "Notizen");
    i.set_locale("ja");
    assert_eq!(i.text("app.title"), "Notes");
    assert_eq!(i.text("no.such.key"), "no.such.key");
    assert_eq!(i.get("no.such.key"), None);
    assert!(!i.set_locale("C"));
    assert_eq!(i.locale().as_str(), "ja");
}

#[test]
fn plurals_use_each_locales_rules() {
    let mut i = i18n();
    assert_eq!(i.plural("files", 1, &[]), "1 file");
    assert_eq!(i.plural("files", 1234, &[]), "1,234 files");
    i.set_locale("de");
    assert_eq!(i.plural("files", 1, &[]), "eine Datei");
    assert_eq!(i.plural("files", 2000, &[]), "2.000 Dateien");
    i.set_locale("ru");
    assert_eq!(i.plural("files", 1, &[]), "1 файл");
    assert_eq!(i.plural("files", 3, &[]), "3 файла");
    assert_eq!(i.plural("files", 5, &[]), "5 файлов");
    assert_eq!(i.plural("files", 21, &[]), "21 файл");
    // ru has no "greet": English text, filled
    assert_eq!(i.format("greet", &[("name", "Ада")]), "Hello, Ада!");
    // a key with no plural forms anywhere falls back to the plain key text
    assert_eq!(i.plural("greet", 2, &[("name", "x")]), "Hello, x!");
    assert_eq!(i.plural("nothing", 2, &[]), "nothing");
}

#[test]
fn negotiation_picks_available_catalogs() {
    let mut i = i18n().with("zh-Hant", r#"{"app.title": "筆記"}"#).with("zh-Hans", r#"{"app.title": "笔记"}"#);
    let best = i.negotiate(&["zh-TW", "en"]);
    assert_eq!(best, "zh-Hant");
    i.set_locale(&best);
    assert_eq!(i.text("app.title"), "筆記");
    assert_eq!(i.negotiate(&["de_DE.UTF-8"]), "de");
    assert_eq!(i.negotiate(&["fr", "C", ""]), "en");
    assert_eq!(i.negotiate::<&str>(&[]), "en");
}

#[test]
fn bad_catalogs_never_panic_and_are_reported() {
    let mut i = I18n::new("en").with("en", EN).with("fr", "{ not json").with("es", "[]").with("not a tag!", "{}").with("it", r#"{"app.title": 5}"#);
    i.set_locale("fr");
    assert_eq!(i.text("app.title"), "Notes");
    i.set_locale("it");
    assert_eq!(i.plural("files", 2, &[]), "2 files");
    let problems = i.problems();
    assert_eq!(problems.len(), 4, "{problems:?}");
    assert!(problems.iter().any(|p| matches!(p, Problem::BadLocale(_))));
    // the empty catalog is not a crash either
    let empty = I18n::new("");
    assert_eq!(empty.source().as_str(), "en");
    assert_eq!(empty.text("x"), "x");
}

#[test]
fn check_finds_missing_keys_and_placeholders() {
    assert_eq!(i18n().with("ru", r#"{"app.title":"Заметки","greet":"Привет, {name}!","only.en":"x"}"#).check(), vec![]);
    let i = I18n::new("en").with("en", EN).with("fr", r#"{"app.title": "Notes", "greet": "Bonjour, {nom} !", "files.one": "{count} fichier"}"#);
    let mut got: Vec<String> = i.check().iter().map(ToString::to_string).collect();
    got.sort();
    assert_eq!(got, ["fr: greet has different placeholders than the source", "fr: missing files.other", "fr: missing only.en"]);
    assert_eq!(i.missing_source_keys(&["app.title", "files", "typo"]), ["typo"]);
}

#[test]
fn layers_override_key_by_key() {
    let mut i = I18n::new("en").with("en", EN).with("en", r#"{"app.title": "My Notes"}"#);
    assert_eq!(i.text("app.title"), "My Notes");
    assert_eq!(i.text("only.en"), "English only");
    i.set_locale("en-GB");
    assert_eq!(i.decimal(1234.5, 1), "1,234.5");
}
