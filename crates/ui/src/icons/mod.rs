//! Catalogs of trusted, compiled-in monochrome SVG icons.
//!
//! A catalog is prepared once (colour and stroke width substituted as plain text) and then hands
//! egui the same shared bytes under a stable URI for every lookup. It installs no image loader,
//! reads no files and does not parse SVG: the app installs egui_extras' SVG loader and sets size
//! and tint on the `egui::Image`.

use std::collections::HashMap;
use std::sync::Arc;

use egui::ImageSource;
use egui::load::Bytes;

/// The substitutions applied to every icon of a catalog.
#[derive(Clone, Copy, Debug)]
pub struct SvgStyle {
    /// Replaces each literal `currentColor`.
    pub current_color: &'static str,
    /// When set, replaces each exact `stroke-width="2"`.
    pub stroke_width: Option<&'static str>,
}

/// An immutable set of prepared icons.
pub struct SvgIconSet {
    icons: HashMap<&'static str, Arc<[u8]>>,
    uri_prefix: &'static str,
    fallback: &'static str,
    empty: Arc<[u8]>,
}

impl SvgIconSet {
    /// Prepare `icons` (name, SVG bytes) with `style`. URIs are `uri_prefix` + name + `.svg`, so
    /// use a distinct prefix for each catalog and style. `fallback` names the icon served for
    /// unknown names. Later duplicates replace earlier ones; invalid UTF-8 is replaced.
    pub fn new(icons: &'static [(&'static str, &'static [u8])], style: SvgStyle, uri_prefix: &'static str, fallback: &'static str) -> Self {
        let mut prepared = HashMap::with_capacity(icons.len());
        for &(name, svg) in icons {
            let mut text = String::from_utf8_lossy(svg).replace("currentColor", style.current_color);
            if let Some(width) = style.stroke_width {
                text = text.replace("stroke-width=\"2\"", &format!("stroke-width=\"{width}\""));
            }
            prepared.insert(name, Arc::from(text.into_bytes()));
        }
        Self { icons: prepared, uri_prefix, fallback, empty: Arc::from(Vec::new()) }
    }

    /// Whether `name` is in the catalog (the fallback does not count).
    pub fn contains(&self, name: &str) -> bool {
        self.icons.contains_key(name)
    }

    /// The catalog's own key for `name`, if present. Resolving names first lets an app keep one
    /// cache entry per real icon.
    pub fn canonical_name(&self, name: &str) -> Option<&'static str> {
        self.icons.get_key_value(name).map(|(&key, _)| key)
    }

    /// The image source for `name`: its URI always uses the requested name; its bytes are the
    /// icon, else the fallback icon, else empty (which the loader then reports).
    pub fn source(&self, name: &str) -> ImageSource<'static> {
        let bytes = self.icons.get(name).or_else(|| self.icons.get(self.fallback)).unwrap_or(&self.empty);
        ImageSource::Bytes { uri: format!("{}{name}.svg", self.uri_prefix).into(), bytes: Bytes::Shared(Arc::clone(bytes)) }
    }
}

#[cfg(test)]
mod tests;
