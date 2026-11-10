//! CLDR cardinal plural rules for common languages, as a small table instead of a full rules
//! engine. Languages not listed use `other` for everything except where noted.

/// A CLDR plural category; catalogs name plural forms `key.zero`, `key.one`, … `key.other`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Category {
    Zero,
    One,
    Two,
    Few,
    Many,
    Other,
}

impl Category {
    /// The suffix catalogs use: `zero`, `one`, `two`, `few`, `many`, `other`.
    pub fn as_str(self) -> &'static str {
        match self {
            Category::Zero => "zero",
            Category::One => "one",
            Category::Two => "two",
            Category::Few => "few",
            Category::Many => "many",
            Category::Other => "other",
        }
    }
}

/// The operands CLDR rules are written in: `i` integer digits, `v` visible fraction digit count,
/// `f` visible fraction digits as an integer. `n` is `i` plus the fraction.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Operands {
    pub i: u64,
    pub v: u32,
    pub f: u64,
}

impl Operands {
    /// An integer count (the sign is ignored, as in CLDR).
    pub fn int(n: i64) -> Self {
        Operands { i: n.unsigned_abs(), v: 0, f: 0 }
    }

    /// A number as it is shown, e.g. `"1.50"` (`i`=1, `v`=2, `f`=50). `None` when it is not a
    /// plain decimal.
    pub fn parse(shown: &str) -> Option<Self> {
        let s = shown.trim().trim_start_matches(['-', '+']);
        let (int, frac) = s.split_once('.').unwrap_or((s, ""));
        if int.is_empty() || !int.bytes().all(|b| b.is_ascii_digit()) || !frac.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        // keep the low digits: rules only look at i % 1_000_000 and below
        let low = |d: &str| d.get(d.len().saturating_sub(18)..).unwrap_or(d).parse::<u64>().ok();
        let i = low(int)?;
        let f = if frac.is_empty() { 0 } else { low(frac)? };
        Some(Operands { i, v: u32::try_from(frac.len()).unwrap_or(u32::MAX), f })
    }

    /// `n` when it is a whole number.
    fn whole(self) -> Option<u64> {
        (self.f == 0).then_some(self.i)
    }
}

/// The plural category of `n` in `language` (a language subtag such as `ru`; a full tag such as
/// `pt-PT` also works where the region matters).
pub fn category(locale: &str, n: Operands) -> Category {
    let mut parts = locale.split('-');
    let language = parts.next().unwrap_or("");
    let region = parts.find(|p| p.len() == 2);
    let Operands { i, v, .. } = n;
    let int = v == 0;
    let (i10, i100) = (i % 10, i % 100);
    match language {
        // no plural forms
        "ja" | "zh" | "ko" | "th" | "vi" | "id" | "ms" | "my" | "lo" | "km" | "yue" => Category::Other,
        // one: i = 1 and v = 0
        "en" | "de" | "nl" | "sv" | "nb" | "nn" | "no" | "fi" | "et" | "it" | "ca" | "gl" | "ur" | "sw" => {
            if i == 1 && int {
                Category::One
            } else {
                Category::Other
            }
        }
        "pt" if region == Some("PT") => {
            if i == 1 && int {
                Category::One
            } else if int && i != 0 && i % 1_000_000 == 0 {
                Category::Many
            } else {
                Category::Other
            }
        }
        // one: i = 0,1; many: a whole million
        "fr" | "pt" => {
            if i <= 1 {
                Category::One
            } else if int && i != 0 && i % 1_000_000 == 0 {
                Category::Many
            } else {
                Category::Other
            }
        }
        "es" => {
            if n.whole() == Some(1) {
                Category::One
            } else if int && i != 0 && i % 1_000_000 == 0 {
                Category::Many
            } else {
                Category::Other
            }
        }
        // one: n = 1
        "tr" | "hu" | "el" | "bg" | "az" | "ka" | "kk" | "uz" | "sq" => {
            if n.whole() == Some(1) {
                Category::One
            } else {
                Category::Other
            }
        }
        // one: i = 0 or n = 1
        "hi" | "bn" | "fa" | "gu" | "kn" | "mr" | "zu" | "am" => {
            if i == 0 || n.whole() == Some(1) {
                Category::One
            } else {
                Category::Other
            }
        }
        "da" => {
            if n.whole() == Some(1) || (n.f != 0 && i <= 1) {
                Category::One
            } else {
                Category::Other
            }
        }
        "ru" | "uk" | "be" => {
            if !int {
                Category::Other
            } else if i10 == 1 && i100 != 11 {
                Category::One
            } else if (2..=4).contains(&i10) && !(12..=14).contains(&i100) {
                Category::Few
            } else {
                Category::Many
            }
        }
        "pl" => {
            if !int {
                Category::Other
            } else if i == 1 {
                Category::One
            } else if (2..=4).contains(&i10) && !(12..=14).contains(&i100) {
                Category::Few
            } else {
                Category::Many
            }
        }
        "cs" | "sk" => {
            if !int {
                Category::Many
            } else if i == 1 {
                Category::One
            } else if (2..=4).contains(&i) {
                Category::Few
            } else {
                Category::Other
            }
        }
        "he" | "iw" => {
            if (i == 1 && int) || (i == 0 && !int) {
                Category::One
            } else if i == 2 && int {
                Category::Two
            } else {
                Category::Other
            }
        }
        "ar" => match n.whole() {
            Some(0) => Category::Zero,
            Some(1) => Category::One,
            Some(2) => Category::Two,
            Some(w) if (3..=10).contains(&(w % 100)) => Category::Few,
            Some(w) if (11..=99).contains(&(w % 100)) => Category::Many,
            _ => Category::Other,
        },
        _ => {
            if i == 1 && int {
                Category::One
            } else {
                Category::Other
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Category::*;
    use super::*;

    fn int(locale: &str, n: i64) -> Category {
        category(locale, Operands::int(n))
    }

    fn dec(locale: &str, shown: &str) -> Category {
        category(locale, Operands::parse(shown).unwrap_or_default())
    }

    #[test]
    fn english() {
        assert_eq!([0, 1, 2, 11, 21, -1].map(|n| int("en", n)), [Other, One, Other, Other, Other, One]);
        assert_eq!(dec("en", "1.0"), Other);
        assert_eq!(dec("en", "1"), One);
    }

    #[test]
    fn french() {
        assert_eq!([0, 1, 2, 1_000_000, 2_000_000, 1_000_001].map(|n| int("fr", n)), [One, One, Other, Many, Many, Other]);
        assert_eq!(dec("fr", "1.5"), One);
        assert_eq!(dec("fr", "2.5"), Other);
    }

    #[test]
    fn russian() {
        let got = [1, 2, 4, 5, 11, 12, 14, 21, 22, 25, 101, 111, 0].map(|n| int("ru", n));
        assert_eq!(got, [One, Few, Few, Many, Many, Many, Many, One, Few, Many, One, Many, Many]);
        assert_eq!(dec("ru", "1.5"), Other);
    }

    #[test]
    fn arabic() {
        let got = [0, 1, 2, 3, 10, 11, 99, 100, 102, 103, 111].map(|n| int("ar", n));
        assert_eq!(got, [Zero, One, Two, Few, Few, Many, Many, Other, Other, Few, Many]);
        assert_eq!(dec("ar", "1.5"), Other);
    }

    #[test]
    fn japanese() {
        assert_eq!([0, 1, 2, 100].map(|n| int("ja", n)), [Other; 4]);
    }

    #[test]
    fn others() {
        assert_eq!([1, 2, 5, 12, 22, 25].map(|n| int("pl", n)), [One, Few, Many, Many, Few, Many]);
        assert_eq!([1, 3, 5].map(|n| int("cs", n)), [One, Few, Other]);
        assert_eq!(dec("cs", "1.5"), Many);
        assert_eq!([0, 1].map(|n| int("pt", n)), [One, One]);
        assert_eq!([0, 1].map(|n| int("pt-PT", n)), [Other, One]);
        assert_eq!([1, 2, 3].map(|n| int("he", n)), [One, Two, Other]);
        assert_eq!(int("xx", 1), One);
        assert_eq!(int("en", i64::MIN), Other);
    }

    #[test]
    fn operands_are_hostile_safe() {
        assert_eq!(Operands::parse("1.50"), Some(Operands { i: 1, v: 2, f: 50 }));
        assert_eq!(Operands::parse("99999999999999999999999999.1").map(|o| o.v), Some(1));
        for bad in ["", ".5", "1e5", "abc", "1.2.3", "--"] {
            assert_eq!(Operands::parse(bad), None, "{bad:?}");
        }
    }
}
