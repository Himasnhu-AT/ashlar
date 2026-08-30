//! Install font families from bytes the app supplies, with a CJK fallback chain.
//!
//! ashlar ships no font files. The app embeds or loads its own (for example Inter and JetBrains
//! Mono, plus Noto or BIZ UD faces for Japanese, Chinese and Korean) and hands their bytes to a
//! [`FontInstall`]:
//!
//! ```no_run
//! # use ashlar_ui::fonts::{CjkScript, FontInstall};
//! # static INTER: &[u8] = &[]; static MONO: &[u8] = &[]; static JP: &[u8] = &[]; static SC: &[u8] = &[];
//! let ctx = egui::Context::default();
//! let mut fonts = FontInstall::new();
//! fonts.add("inter", INTER)?.add("mono", MONO)?;
//! fonts.primary(egui::FontFamily::Proportional, "inter")?;
//! fonts.primary(egui::FontFamily::Monospace, "mono")?;
//! fonts.add_cjk(CjkScript::Japanese, "noto-jp", JP)?;
//! fonts.add_cjk(CjkScript::SimplifiedChinese, "noto-sc", SC)?;
//! fonts.prefer(CjkScript::Japanese); // the UI language decides which Han glyph shapes win
//! fonts.install(&ctx);
//! # Ok::<(), ashlar_ui::fonts::FontError>(())
//! ```
//!
//! Every face is parsed when added, so a corrupt file is an error here instead of a panic inside
//! egui on the next frame. Fallbacks are appended to every family at install time, in order:
//! general fallbacks first (emoji, symbols), then CJK faces with the preferred script first.

use std::sync::Arc;

use egui::{FontData, FontDefinitions, FontFamily};

/// Faces bigger than this are refused (the largest CJK faces are around 25 MB).
pub const MAX_FONT_BYTES: usize = 64 * 1024 * 1024;

/// Why a face or family was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FontError {
    #[error("font `{0}` is not a readable TrueType/OpenType face")]
    Unreadable(String),
    #[error("font `{0}` is larger than {MAX_FONT_BYTES} bytes")]
    TooLarge(String),
    #[error("font `{0}` was not added")]
    Unknown(String),
    #[error("font name must not be empty")]
    EmptyName,
}

/// The script a CJK face is designed for. Han characters are shared between the four, but their
/// preferred glyph shapes differ, so the face for the UI's language should come first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CjkScript {
    Japanese,
    SimplifiedChinese,
    TraditionalChinese,
    Korean,
}

impl CjkScript {
    /// The preferred script for a BCP 47 language tag (`ja`, `zh-Hant-TW`, `ko-KR`…), if it is a
    /// CJK language.
    pub fn for_language(tag: &str) -> Option<Self> {
        let lower = tag.to_ascii_lowercase();
        let mut parts = lower.split(['-', '_']);
        match parts.next()? {
            "ja" => Some(CjkScript::Japanese),
            "ko" => Some(CjkScript::Korean),
            "zh" => {
                let rest: Vec<&str> = parts.collect();
                let traditional = rest.iter().any(|p| matches!(*p, "hant" | "tw" | "hk" | "mo"));
                Some(if traditional { CjkScript::TraditionalChinese } else { CjkScript::SimplifiedChinese })
            }
            _ => None,
        }
    }
}

/// Font bytes: embedded (`&'static`) or loaded at run time.
pub enum FontBytes {
    Static(&'static [u8]),
    Owned(Vec<u8>),
}

impl From<&'static [u8]> for FontBytes {
    fn from(b: &'static [u8]) -> Self {
        FontBytes::Static(b)
    }
}

impl From<Vec<u8>> for FontBytes {
    fn from(b: Vec<u8>) -> Self {
        FontBytes::Owned(b)
    }
}

impl FontBytes {
    fn as_slice(&self) -> &[u8] {
        match self {
            FontBytes::Static(b) => b,
            FontBytes::Owned(b) => b,
        }
    }
}

/// A font setup under construction. See the [module docs](self).
pub struct FontInstall {
    defs: FontDefinitions,
    fallbacks: Vec<String>,
    cjk: Vec<(CjkScript, String)>,
    preferred: Option<CjkScript>,
}

impl Default for FontInstall {
    fn default() -> Self {
        Self::new()
    }
}

impl FontInstall {
    /// Start from egui's defaults (its bundled fonts when the app enables egui's `default_fonts`
    /// feature, none otherwise).
    pub fn new() -> Self {
        Self::from_definitions(FontDefinitions::default())
    }

    /// Start with no faces at all.
    pub fn empty() -> Self {
        Self::from_definitions(FontDefinitions::empty())
    }

    /// Start from existing definitions.
    pub fn from_definitions(defs: FontDefinitions) -> Self {
        Self { defs, fallbacks: Vec::new(), cjk: Vec::new(), preferred: None }
    }

    /// Add a face under `name` (validated now). It is not used until a family lists it.
    pub fn add(&mut self, name: &str, bytes: impl Into<FontBytes>) -> Result<&mut Self, FontError> {
        self.add_indexed(name, bytes, 0)
    }

    /// Add face `index` of a collection (`.ttc`).
    pub fn add_indexed(&mut self, name: &str, bytes: impl Into<FontBytes>, index: u32) -> Result<&mut Self, FontError> {
        if name.is_empty() {
            return Err(FontError::EmptyName);
        }
        let bytes = bytes.into();
        if bytes.as_slice().len() > MAX_FONT_BYTES {
            return Err(FontError::TooLarge(name.to_owned()));
        }
        if skrifa::FontRef::from_index(bytes.as_slice(), index).is_err() {
            return Err(FontError::Unreadable(name.to_owned()));
        }
        let mut data = match bytes {
            FontBytes::Static(b) => FontData::from_static(b),
            FontBytes::Owned(b) => FontData::from_owned(b),
        };
        data.index = index;
        self.defs.font_data.insert(name.to_owned(), Arc::new(data));
        Ok(self)
    }

    fn known(&self, name: &str) -> Result<(), FontError> {
        if self.defs.font_data.contains_key(name) { Ok(()) } else { Err(FontError::Unknown(name.to_owned())) }
    }

    /// Make an added face the first choice of `family` (keeping the rest as fallbacks).
    pub fn primary(&mut self, family: FontFamily, name: &str) -> Result<&mut Self, FontError> {
        self.known(name)?;
        let stack = self.defs.families.entry(family).or_default();
        stack.retain(|n| n != name);
        stack.insert(0, name.to_owned());
        Ok(self)
    }

    /// Define `family` (typically `FontFamily::Name("semibold".into())`) as exactly these faces.
    pub fn family(&mut self, family: FontFamily, names: &[&str]) -> Result<&mut Self, FontError> {
        for name in names {
            self.known(name)?;
        }
        self.defs.families.insert(family, names.iter().map(|n| (*n).to_owned()).collect());
        Ok(self)
    }

    /// Add a face appended to every family (emoji, symbols, a wide-coverage face).
    pub fn fallback(&mut self, name: &str, bytes: impl Into<FontBytes>) -> Result<&mut Self, FontError> {
        self.add(name, bytes)?;
        self.fallbacks.retain(|n| n != name);
        self.fallbacks.push(name.to_owned());
        Ok(self)
    }

    /// Add a CJK face to the fallback chain of every family.
    pub fn add_cjk(&mut self, script: CjkScript, name: &str, bytes: impl Into<FontBytes>) -> Result<&mut Self, FontError> {
        self.add(name, bytes)?;
        self.cjk.retain(|(_, n)| n != name);
        self.cjk.push((script, name.to_owned()));
        Ok(self)
    }

    /// Which script's faces come first in the CJK chain (default: the order they were added).
    pub fn prefer(&mut self, script: CjkScript) -> &mut Self {
        self.preferred = Some(script);
        self
    }

    /// The fallback chain appended to each family: general fallbacks, then CJK faces.
    pub fn fallback_chain(&self) -> Vec<&str> {
        let mut cjk: Vec<&(CjkScript, String)> = self.cjk.iter().collect();
        if let Some(p) = self.preferred {
            // stable: keeps the added order within each group
            cjk.sort_by_key(|(s, _)| *s != p);
        }
        self.fallbacks.iter().map(String::as_str).chain(cjk.into_iter().map(|(_, n)| n.as_str())).collect()
    }

    /// The finished definitions. Every family references only added faces.
    pub fn definitions(&self) -> FontDefinitions {
        let mut defs = self.defs.clone();
        let chain = self.fallback_chain();
        for stack in defs.families.values_mut() {
            for name in &chain {
                if !stack.iter().any(|n| n == name) {
                    stack.push((*name).to_owned());
                }
            }
            stack.retain(|n| defs.font_data.contains_key(n));
        }
        for family in [FontFamily::Proportional, FontFamily::Monospace] {
            defs.families.entry(family).or_insert_with(|| chain.iter().map(|n| (*n).to_owned()).collect());
        }
        defs
    }

    /// Install into the context (takes effect on the next frame).
    pub fn install(&self, ctx: &egui::Context) {
        ctx.set_fonts(self.definitions());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// egui's bundled faces (dev-dependency feature), so no font files live in this repo.
    fn bundled(name: &str) -> Vec<u8> {
        let defs = FontDefinitions::default();
        defs.font_data.get(name).map(|d| d.font.to_vec()).unwrap_or_default()
    }

    fn any_bundled() -> (String, Vec<u8>) {
        let defs = FontDefinitions::default();
        let (name, data) = defs.font_data.iter().next().map(|(n, d)| (n.clone(), d.font.to_vec())).unwrap_or_default();
        (name, data)
    }

    #[test]
    fn corrupt_or_unknown_faces_are_errors_not_panics() {
        let mut f = FontInstall::empty();
        assert_eq!(f.add("bad", vec![0u8; 64]).err(), Some(FontError::Unreadable("bad".into())));
        assert_eq!(f.add("", vec![]).err(), Some(FontError::EmptyName));
        assert_eq!(f.primary(FontFamily::Proportional, "missing").err(), Some(FontError::Unknown("missing".into())));
        assert_eq!(f.family(FontFamily::Name("x".into()), &["missing"]).err(), Some(FontError::Unknown("missing".into())));
        // nothing half-installed: the definitions still render
        let ctx = egui::Context::default();
        f.install(&ctx);
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            ui.label("still works");
        });
        out.textures_delta.clear();
    }

    #[test]
    fn faces_install_and_cjk_chain_puts_the_preferred_script_first() {
        let (_, face) = any_bundled();
        assert!(!face.is_empty(), "egui default_fonts is enabled for tests");
        let mut f = FontInstall::new();
        f.add("ui", face.clone()).unwrap().primary(FontFamily::Proportional, "ui").unwrap();
        f.add_cjk(CjkScript::SimplifiedChinese, "sc", face.clone()).unwrap();
        f.add_cjk(CjkScript::Japanese, "jp", face.clone()).unwrap();
        f.fallback("emoji", face.clone()).unwrap();
        assert_eq!(f.fallback_chain(), ["emoji", "sc", "jp"]);
        f.prefer(CjkScript::Japanese);
        assert_eq!(f.fallback_chain(), ["emoji", "jp", "sc"]);
        f.family(FontFamily::Name("semibold".into()), &["ui"]).unwrap();
        let defs = f.definitions();
        let prop = &defs.families[&FontFamily::Proportional];
        assert_eq!(prop.first().map(String::as_str), Some("ui"));
        assert!(prop.ends_with(&["emoji".to_owned(), "jp".to_owned(), "sc".to_owned()]), "{prop:?}");
        assert_eq!(defs.families[&FontFamily::Name("semibold".into())], ["ui", "emoji", "jp", "sc"]);
        let ctx = egui::Context::default();
        f.install(&ctx);
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            ui.label(egui::RichText::new("semibold").family(FontFamily::Name("semibold".into())));
        });
        out.textures_delta.clear();
        assert!(ctx.fonts(|fonts| fonts.definitions().font_data.contains_key("jp")));
    }

    #[test]
    fn empty_install_still_defines_the_two_base_families() {
        let face = bundled("Hack");
        let mut f = FontInstall::empty();
        if !face.is_empty() {
            f.fallback("hack", face).unwrap();
        }
        let defs = f.definitions();
        assert!(defs.families.contains_key(&FontFamily::Proportional));
        assert!(defs.families.contains_key(&FontFamily::Monospace));
    }

    #[test]
    fn language_tags_choose_a_script() {
        assert_eq!(CjkScript::for_language("ja-JP"), Some(CjkScript::Japanese));
        assert_eq!(CjkScript::for_language("zh"), Some(CjkScript::SimplifiedChinese));
        assert_eq!(CjkScript::for_language("zh-Hant"), Some(CjkScript::TraditionalChinese));
        assert_eq!(CjkScript::for_language("zh_TW"), Some(CjkScript::TraditionalChinese));
        assert_eq!(CjkScript::for_language("ko"), Some(CjkScript::Korean));
        assert_eq!(CjkScript::for_language("en-US"), None);
        assert_eq!(CjkScript::for_language(""), None);
    }
}
