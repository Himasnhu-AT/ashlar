//! Locale tags: parsing (BCP 47 and POSIX), fallback chains, negotiation and the system's
//! preferred languages.

use std::fmt;

/// Longest tag accepted, in bytes; longer input is not a locale.
const MAX_TAG: usize = 64;
/// Most preferred languages read from the environment.
const MAX_PREFERRED: usize = 16;

/// A normalized language tag: `language[-Script][-REGION]`, e.g. `en`, `de-DE`, `zh-Hant-TW`.
///
/// Variants and extensions are dropped; they never change which catalog is picked here.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Locale(Box<str>);

impl Locale {
    /// Parses a BCP 47 tag (`de-DE`, `zh_Hant_TW`) or a POSIX locale name (`de_DE.UTF-8@euro`).
    ///
    /// Returns `None` for `C`, `POSIX`, empty or malformed input.
    pub fn parse(input: &str) -> Option<Locale> {
        let s = input.trim();
        if s.is_empty() || s.len() > MAX_TAG {
            return None;
        }
        // POSIX: language[_territory][.codeset][@modifier]
        let s = s.split(['.', '@']).next().unwrap_or("");
        if s.eq_ignore_ascii_case("c") || s.eq_ignore_ascii_case("posix") {
            return None;
        }
        let mut parts = s.split(['-', '_']);
        let language = parts.next().filter(|l| (2..=3).contains(&l.len()) && l.bytes().all(|b| b.is_ascii_alphabetic()))?;
        let mut out = language.to_ascii_lowercase();
        let mut script_seen = false;
        let mut region_seen = false;
        for part in parts {
            if part.is_empty() || !part.bytes().all(|b| b.is_ascii_alphanumeric()) {
                return None;
            }
            let alpha = part.bytes().all(|b| b.is_ascii_alphabetic());
            if !script_seen && !region_seen && part.len() == 4 && alpha {
                out.push('-');
                let mut chars = part.chars();
                if let Some(first) = chars.next() {
                    out.push(first.to_ascii_uppercase());
                }
                out.extend(chars.map(|c| c.to_ascii_lowercase()));
                script_seen = true;
            } else if !region_seen && ((part.len() == 2 && alpha) || (part.len() == 3 && part.bytes().all(|b| b.is_ascii_digit()))) {
                out.push('-');
                out.push_str(&part.to_ascii_uppercase());
                region_seen = true;
            } else {
                // a variant, an extension or a private-use part: the rest does not pick a catalog
                break;
            }
        }
        Some(Locale(out.into_boxed_str()))
    }

    /// English (`en`).
    pub fn english() -> Locale {
        Locale("en".into())
    }

    /// The tag, e.g. `de-DE`.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The language subtag, e.g. `de`.
    pub fn language(&self) -> &str {
        self.0.split('-').next().unwrap_or("")
    }

    /// The script subtag, e.g. `Hant`.
    pub fn script(&self) -> Option<&str> {
        self.0.split('-').skip(1).find(|p| p.len() == 4)
    }

    /// The region subtag, e.g. `DE` or `419`.
    pub fn region(&self) -> Option<&str> {
        self.0.split('-').skip(1).find(|p| p.len() == 2 || p.len() == 3)
    }

    /// The tags to try, most specific first: `zh-TW` gives `zh-TW`, `zh-Hant-TW`, `zh-Hant`, `zh`;
    /// `de-AT` gives `de-AT`, `de`. The source locale is not included.
    pub fn fallbacks(&self) -> Vec<Locale> {
        let mut out = vec![self.clone()];
        let language = self.language();
        let script = self.script().map(str::to_owned).or_else(|| likely_script(language, self.region()).map(str::to_owned));
        if let Some(script) = &script {
            if let Some(region) = self.region() {
                push_unique(&mut out, format!("{language}-{script}-{region}"));
            }
            push_unique(&mut out, format!("{language}-{script}"));
        }
        if let Some(region) = self.region() {
            push_unique(&mut out, format!("{language}-{region}"));
        }
        push_unique(&mut out, language.to_owned());
        out
    }
}

fn push_unique(out: &mut Vec<Locale>, tag: String) {
    if !out.iter().any(|l| *l.0 == *tag) {
        out.push(Locale(tag.into_boxed_str()));
    }
}

/// The script a language is written in when a region implies it (only where it picks a different
/// catalog: Chinese and Serbian).
fn likely_script(language: &str, region: Option<&str>) -> Option<&'static str> {
    match (language, region) {
        ("zh", Some("TW" | "HK" | "MO")) => Some("Hant"),
        ("zh", _) => Some("Hans"),
        ("sr", Some("ME")) => Some("Latn"),
        ("sr", _) => Some("Cyrl"),
        _ => None,
    }
}

impl fmt::Display for Locale {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Picks the best of `available` for a user who prefers `preferred` (most preferred first).
///
/// Each preferred tag is tried in turn: first its own fallback chain (`zh-TW` matches an available
/// `zh-Hant`), then any available locale of the same language (`en` matches `en-GB`). Returns
/// `None` when nothing matches; callers then use their source locale.
pub fn negotiate<'a>(preferred: &[Locale], available: &'a [Locale]) -> Option<&'a Locale> {
    for want in preferred {
        for candidate in want.fallbacks() {
            if let Some(found) = available.iter().find(|a| **a == candidate) {
                return Some(found);
            }
        }
        // same language, any region: but never across scripts (zh-Hant must not get zh-Hans)
        let script = want.script().map(str::to_owned).or_else(|| likely_script(want.language(), want.region()).map(str::to_owned));
        let same = available.iter().find(|a| {
            a.language() == want.language()
                && match (&script, a.script().or_else(|| likely_script(a.language(), a.region()))) {
                    (Some(w), Some(s)) => w == s,
                    _ => true,
                }
        });
        if let Some(found) = same {
            return Some(found);
        }
    }
    None
}

/// The OS's own language list, installed by [`set_native_languages`].
static NATIVE: std::sync::OnceLock<fn() -> Vec<String>> = std::sync::OnceLock::new();

/// Install where [`system_locales`] reads the OS's preferred UI languages (`ashlar-app` installs
/// `ashlar_native::system::preferred_languages`, which reads macOS `NSLocale.preferredLanguages`
/// and Windows `GetUserPreferredUILanguages`). The first call wins; returns whether it did.
pub fn set_native_languages(source: fn() -> Vec<String>) -> bool {
    NATIVE.set(source).is_ok()
}

/// The user's preferred languages, most preferred first: the environment's (`LANGUAGE`,
/// `LC_ALL`, `LC_MESSAGES`, `LANG`), then the OS's list when one is installed
/// ([`set_native_languages`]; desktop launches on macOS and Windows often have no `LANG`). Empty
/// when neither says anything (e.g. in a browser).
pub fn system_locales() -> Vec<Locale> {
    let native = NATIVE.get().map(|f| f()).unwrap_or_default();
    with_native(from_env(|name| std::env::var(name).ok()), &native)
}

/// `env` followed by the parsable, new tags of `native`, at most [`MAX_PREFERRED`] in all.
pub fn with_native<S: AsRef<str>>(env: Vec<Locale>, native: &[S]) -> Vec<Locale> {
    let mut out = env;
    for tag in native.iter().take(MAX_PREFERRED) {
        if out.len() >= MAX_PREFERRED {
            break;
        }
        if let Some(l) = Locale::parse(tag.as_ref())
            && !out.contains(&l)
        {
            out.push(l);
        }
    }
    out
}

/// [`system_locales`] over any variable source (tests pass a map).
pub fn from_env(get: impl Fn(&str) -> Option<String>) -> Vec<Locale> {
    let set = |name: &str| get(name).filter(|v| !v.trim().is_empty());
    let primary = set("LC_ALL").or_else(|| set("LC_MESSAGES")).or_else(|| set("LANG"));
    let mut out: Vec<Locale> = Vec::new();
    // GNU gettext: LANGUAGE is a priority list, ignored when the locale is "C"
    let c_locale = primary.as_deref().is_some_and(|p| Locale::parse(p).is_none());
    if !c_locale && let Some(list) = set("LANGUAGE") {
        for tag in list.split(':').take(MAX_PREFERRED) {
            if let Some(l) = Locale::parse(tag)
                && !out.contains(&l)
            {
                out.push(l);
            }
        }
    }
    if let Some(l) = primary.as_deref().and_then(Locale::parse)
        && !out.contains(&l)
    {
        out.push(l);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_languages_follow_the_environment() {
        let env = from_env(|n| (n == "LANG").then(|| "de_DE.UTF-8".to_string()));
        let all = with_native(env, &["fr-CA", "de-DE", "not a tag!", "zh-Hans-CN"]);
        let tags: Vec<String> = all.iter().map(|l| l.as_str().to_string()).collect();
        assert_eq!(tags.first().map(String::as_str), Some("de-DE"));
        assert!(tags.iter().any(|t| t == "fr-CA") && tags.iter().any(|t| t.starts_with("zh")), "{tags:?}");
        assert_eq!(tags.iter().filter(|t| *t == "de-DE").count(), 1, "no duplicates: {tags:?}");
        let many: Vec<String> = (0..100).map(|i| format!("x{}", (b'a' + (i % 26) as u8) as char)).collect();
        assert!(with_native(Vec::new(), &many).len() <= MAX_PREFERRED);
    }

    fn tag(s: &str) -> Locale {
        Locale::parse(s).unwrap_or_else(|| Locale("invalid".into()))
    }

    #[test]
    fn parses_posix_and_bcp47() {
        assert_eq!(tag("de_DE.UTF-8").as_str(), "de-DE");
        assert_eq!(tag("de_DE.UTF-8@euro").as_str(), "de-DE");
        assert_eq!(tag("sr_RS@latin").as_str(), "sr-RS");
        assert_eq!(tag("ZH-hant-tw").as_str(), "zh-Hant-TW");
        assert_eq!(tag("es-419").as_str(), "es-419");
        assert_eq!(tag("en-US-x-private").as_str(), "en-US");
        assert_eq!(tag("pt").as_str(), "pt");
        for bad in ["", "C", "POSIX", "C.UTF-8", "e", "english", "de--DE", "d3", &"a".repeat(200)] {
            assert_eq!(Locale::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn fallback_chains() {
        let chain = |s: &str| tag(s).fallbacks().iter().map(|l| l.to_string()).collect::<Vec<_>>();
        assert_eq!(chain("de-AT"), ["de-AT", "de"]);
        assert_eq!(chain("zh-TW"), ["zh-TW", "zh-Hant-TW", "zh-Hant", "zh"]);
        assert_eq!(chain("zh-CN"), ["zh-CN", "zh-Hans-CN", "zh-Hans", "zh"]);
        assert_eq!(chain("zh-Hant-HK"), ["zh-Hant-HK", "zh-Hant", "zh-HK", "zh"]);
        assert_eq!(chain("en"), ["en"]);
    }

    #[test]
    fn negotiation() {
        let available: Vec<Locale> = ["en", "de", "fr-CA", "zh-Hans", "zh-Hant", "pt-BR"].map(tag).to_vec();
        let pick = |prefs: &[&str]| {
            let prefs: Vec<Locale> = prefs.iter().map(|p| tag(p)).collect();
            negotiate(&prefs, &available).map(|l| l.to_string())
        };
        assert_eq!(pick(&["zh-TW"]).as_deref(), Some("zh-Hant"));
        assert_eq!(pick(&["zh-HK"]).as_deref(), Some("zh-Hant"));
        assert_eq!(pick(&["zh-CN"]).as_deref(), Some("zh-Hans"));
        assert_eq!(pick(&["zh"]).as_deref(), Some("zh-Hans"));
        assert_eq!(pick(&["de-CH", "en"]).as_deref(), Some("de"));
        assert_eq!(pick(&["fr-FR"]).as_deref(), Some("fr-CA"));
        assert_eq!(pick(&["pt-PT"]).as_deref(), Some("pt-BR"));
        assert_eq!(pick(&["ja", "de"]).as_deref(), Some("de"));
        assert_eq!(pick(&["ja"]), None);
        assert_eq!(pick(&[]), None);
    }

    #[test]
    fn environment() {
        let env = |vars: &'static [(&'static str, &'static str)]| move |k: &str| vars.iter().find(|(n, _)| *n == k).map(|(_, v)| (*v).to_owned());
        let got = |l: Vec<Locale>| l.iter().map(|l| l.to_string()).collect::<Vec<_>>();
        assert_eq!(got(from_env(env(&[("LANG", "de_DE.UTF-8")]))), ["de-DE"]);
        assert_eq!(got(from_env(env(&[("LANG", "de_DE.UTF-8"), ("LC_ALL", "fr_FR.UTF-8")]))), ["fr-FR"]);
        assert_eq!(got(from_env(env(&[("LANG", "en_US.UTF-8"), ("LC_MESSAGES", "sv_SE")]))), ["sv-SE"]);
        assert_eq!(got(from_env(env(&[("LANG", "pt_BR.UTF-8"), ("LANGUAGE", "pt_BR:pt:en")]))), ["pt-BR", "pt", "en"]);
        assert_eq!(got(from_env(env(&[("LANG", "C"), ("LANGUAGE", "de")]))), Vec::<String>::new());
        assert_eq!(got(from_env(env(&[("LC_ALL", ""), ("LANG", "ja_JP.eucJP")]))), ["ja-JP"]);
        assert!(from_env(env(&[])).is_empty());
    }
}
