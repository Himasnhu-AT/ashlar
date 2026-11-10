//! Integer and decimal formatting with each locale's separators and grouping (Latin digits).

/// Most fraction digits [`decimal`] shows.
pub const MAX_FRACTION_DIGITS: u8 = 20;

/// How a locale writes numbers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Symbols {
    pub group: &'static str,
    pub decimal: char,
    /// Digits in the first group (from the right) and in every later one: `(3, 3)`, or `(3, 2)`
    /// for Indian grouping.
    pub grouping: (u8, u8),
    /// Integer digits needed before any grouping applies: 2 in Spanish and Polish (`1234`,
    /// `12 345`), else 1.
    pub min_grouping: u8,
}

const NBSP: &str = "\u{a0}";
const NNBSP: &str = "\u{202f}";

impl Symbols {
    const fn new(group: &'static str, decimal: char) -> Self {
        Symbols { group, decimal, grouping: (3, 3), min_grouping: 1 }
    }

    /// The symbols for a locale tag (`de-CH`, `fr`, `en-IN`); English for unknown languages.
    pub fn for_locale(locale: &str) -> Self {
        let mut parts = locale.split('-');
        let language = parts.next().unwrap_or("");
        let region = parts.find(|p| p.len() == 2);
        match (language, region) {
            ("de" | "it", Some("CH" | "LI")) => Symbols::new("’", '.'),
            ("en" | "hi" | "bn" | "mr" | "gu" | "ta" | "te", Some("IN")) | ("hi" | "bn" | "mr" | "gu" | "ta" | "te", _) => {
                Symbols { grouping: (3, 2), ..Symbols::new(",", '.') }
            }
            ("pt", Some("PT")) => Symbols { min_grouping: 2, ..Symbols::new(NBSP, ',') },
            ("es", Some("MX" | "US" | "419")) => Symbols::new(",", '.'),
            ("es", _) => Symbols { min_grouping: 2, ..Symbols::new(".", ',') },
            ("pl", _) => Symbols { min_grouping: 2, ..Symbols::new(NBSP, ',') },
            ("fr", Some("CH")) => Symbols::new(NNBSP, '.'),
            ("fr", _) => Symbols::new(NNBSP, ','),
            ("de" | "nl" | "it" | "id" | "tr" | "da" | "pt" | "el" | "ro" | "hr" | "sl" | "sr", _) => Symbols::new(".", ','),
            ("ru" | "uk" | "be" | "cs" | "sk" | "sv" | "fi" | "nb" | "nn" | "no" | "bg" | "hu" | "et" | "lv" | "lt" | "kk", _) => {
                Symbols::new(NBSP, ',')
            }
            _ => Symbols::new(",", '.'),
        }
    }
}

/// `n` with the locale's group separators: `1,234,567` (en), `1.234.567` (de).
pub fn integer(symbols: &Symbols, n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut out = String::with_capacity(digits.len() + 8);
    if n < 0 {
        out.push('-');
    }
    group_into(&mut out, &digits, symbols);
    out
}

/// `value` rounded to `fraction_digits` (at most [`MAX_FRACTION_DIGITS`]) with the locale's
/// separators: `1,234.50` (en), `1.234,50` (de). Not-a-number is `NaN`, infinities are `∞`.
pub fn decimal(symbols: &Symbols, value: f64, fraction_digits: u8) -> String {
    if value.is_nan() {
        return "NaN".to_owned();
    }
    if value.is_infinite() {
        return if value < 0.0 { "-∞".to_owned() } else { "∞".to_owned() };
    }
    let digits = usize::from(fraction_digits.min(MAX_FRACTION_DIGITS));
    let plain = format!("{:.*}", digits, value.abs());
    let (int, frac) = plain.split_once('.').unwrap_or((&plain, ""));
    let mut out = String::with_capacity(plain.len() + 8);
    // no "-0.00": only a sign when a non-zero digit is shown
    if value.is_sign_negative() && plain.bytes().any(|b| (b'1'..=b'9').contains(&b)) {
        out.push('-');
    }
    group_into(&mut out, int, symbols);
    if !frac.is_empty() {
        out.push(symbols.decimal);
        out.push_str(frac);
    }
    out
}

fn group_into(out: &mut String, digits: &str, s: &Symbols) {
    let len = digits.len();
    let (first, rest) = (usize::from(s.grouping.0.max(1)), usize::from(s.grouping.1.max(1)));
    if len < first + usize::from(s.min_grouping.max(1)) {
        out.push_str(digits);
        return;
    }
    // where each group starts, counted from the left
    let head = len - first;
    let lead = head % rest;
    let mut at = 0;
    if lead > 0 {
        out.push_str(digits.get(..lead).unwrap_or(""));
        at = lead;
    }
    while at < head {
        if at > 0 {
            out.push_str(s.group);
        }
        out.push_str(digits.get(at..at + rest).unwrap_or(""));
        at += rest;
    }
    if at > 0 {
        out.push_str(s.group);
    }
    out.push_str(digits.get(head..).unwrap_or(""));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn int(locale: &str, n: i64) -> String {
        integer(&Symbols::for_locale(locale), n)
    }

    fn dec(locale: &str, v: f64, d: u8) -> String {
        decimal(&Symbols::for_locale(locale), v, d)
    }

    #[test]
    fn integers() {
        assert_eq!(int("en", 0), "0");
        assert_eq!(int("en", 999), "999");
        assert_eq!(int("en", 1234), "1,234");
        assert_eq!(int("en", -1_234_567), "-1,234,567");
        assert_eq!(int("de", 1_234_567), "1.234.567");
        assert_eq!(int("fr", 1234), "1\u{202f}234");
        assert_eq!(int("ru", 1234), "1\u{a0}234");
        assert_eq!(int("es", 1234), "1234");
        assert_eq!(int("es", 12345), "12.345");
        assert_eq!(int("pl", 1234), "1234");
        assert_eq!(int("en-IN", 12_345_678), "1,23,45,678");
        assert_eq!(int("de-CH", 1234), "1’234");
        assert_eq!(int("en", i64::MIN), "-9,223,372,036,854,775,808");
    }

    #[test]
    fn decimals() {
        assert_eq!(dec("en", 1234.5, 2), "1,234.50");
        assert_eq!(dec("de", 1234.5, 2), "1.234,50");
        assert_eq!(dec("en", 0.125, 0), "0");
        assert_eq!(dec("en", -0.001, 2), "0.00");
        assert_eq!(dec("en", -1.5, 1), "-1.5");
        assert_eq!(dec("en", f64::NAN, 2), "NaN");
        assert_eq!(dec("en", f64::NEG_INFINITY, 2), "-∞");
        assert_eq!(dec("en", 1.0, 255).len(), 22);
        assert!(dec("en", f64::MAX, 2).ends_with(".00"));
    }
}
