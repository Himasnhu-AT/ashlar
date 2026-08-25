//! The system clipboard: text, RGBA images (arboard) and, on macOS, custom pasteboard types so an
//! app can copy its own data alongside a plain-text fallback.
//!
//! Every call opens the clipboard briefly and closes it again, so nothing is held between calls.
//! On Linux (X11) the clipboard is owned by the process that copied: arboard keeps a helper
//! thread so text stays pasteable for a while; see arboard's docs for the details.

use crate::{NativeError, Result};

/// Largest image accepted, in pixels per side.
pub const MAX_IMAGE_SIDE: u32 = 16_384;
/// Largest custom payload, in bytes.
pub const MAX_CUSTOM_BYTES: usize = 256 * 1024 * 1024;

/// An image as RGBA8, row-major, no padding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl Image {
    /// Check the size against the pixel data; refuse empty and oversized images.
    pub fn validate(&self) -> Result<()> {
        let expected = (self.width as usize).checked_mul(self.height as usize).and_then(|n| n.checked_mul(4));
        if self.width == 0 || self.height == 0 || self.width > MAX_IMAGE_SIDE || self.height > MAX_IMAGE_SIDE {
            return Err(NativeError::Invalid(format!("an image of {}×{} pixels can't be copied", self.width, self.height)));
        }
        if expected != Some(self.rgba.len()) {
            return Err(NativeError::Invalid("the pixel data does not match the image size".into()));
        }
        Ok(())
    }
}

/// A pasteboard type name for custom data: a uniform type identifier on macOS
/// (`com.example.app.clip`) or a MIME type (`application/x-example-clip`). Bounded and printable.
pub fn valid_type(name: &str) -> bool {
    !name.is_empty() && name.len() <= 128 && name.chars().all(|c| c.is_ascii_alphanumeric() || "._-+/".contains(c))
}

#[cfg(not(target_arch = "wasm32"))]
mod imp {
    use std::borrow::Cow;

    use super::*;

    fn map(e: arboard::Error) -> NativeError {
        match e {
            arboard::Error::ContentNotAvailable => NativeError::Os("the clipboard holds no such content".into()),
            arboard::Error::ClipboardNotSupported => NativeError::Unsupported,
            arboard::Error::ClipboardOccupied => NativeError::Os("another application is using the clipboard; try again".into()),
            arboard::Error::ConversionFailure => NativeError::Os("the clipboard content could not be converted".into()),
            // `Unknown` carries a description from the OS; never the clipboard content.
            arboard::Error::Unknown { description } => NativeError::Os(crate::os_text(description)),
            _ => NativeError::Os("the clipboard failed".into()),
        }
    }

    fn open() -> Result<arboard::Clipboard> {
        crate::guarded("the clipboard", || arboard::Clipboard::new().map_err(map))
    }

    /// The clipboard's text, `None` when it holds no text.
    pub fn text() -> Result<Option<String>> {
        let mut c = open()?;
        crate::guarded("the clipboard", || match c.get_text() {
            Ok(t) => Ok(Some(t)),
            Err(arboard::Error::ContentNotAvailable) => Ok(None),
            Err(e) => Err(map(e)),
        })
    }

    pub fn set_text(text: &str) -> Result<()> {
        let mut c = open()?;
        crate::guarded("the clipboard", || c.set_text(text).map_err(map))
    }

    /// The clipboard's image as RGBA8, `None` when it holds no image.
    pub fn image() -> Result<Option<Image>> {
        let mut c = open()?;
        crate::guarded("the clipboard", || match c.get_image() {
            Ok(img) => {
                let (Ok(width), Ok(height)) = (u32::try_from(img.width), u32::try_from(img.height)) else {
                    return Err(NativeError::Os("the clipboard image is too large".into()));
                };
                let image = Image { width, height, rgba: img.bytes.into_owned() };
                image.validate()?;
                Ok(Some(image))
            }
            Err(arboard::Error::ContentNotAvailable) => Ok(None),
            Err(e) => Err(map(e)),
        })
    }

    pub fn set_image(image: &Image) -> Result<()> {
        image.validate()?;
        let mut c = open()?;
        let data = arboard::ImageData { width: image.width as usize, height: image.height as usize, bytes: Cow::Borrowed(&image.rgba) };
        crate::guarded("the clipboard", || c.set_image(data).map_err(map))
    }

    pub fn clear() -> Result<()> {
        let mut c = open()?;
        crate::guarded("the clipboard", || c.clear().map_err(map))
    }
}

#[cfg(target_arch = "wasm32")]
mod imp {
    use super::*;

    pub fn text() -> Result<Option<String>> {
        Err(NativeError::Unsupported)
    }
    pub fn set_text(_: &str) -> Result<()> {
        Err(NativeError::Unsupported)
    }
    pub fn image() -> Result<Option<Image>> {
        Err(NativeError::Unsupported)
    }
    pub fn set_image(_: &Image) -> Result<()> {
        Err(NativeError::Unsupported)
    }
    pub fn clear() -> Result<()> {
        Err(NativeError::Unsupported)
    }
}

pub use imp::{clear, image, set_image, set_text, text};

#[cfg(target_os = "macos")]
mod custom {
    use objc2_app_kit::NSPasteboard;
    use objc2_foundation::{NSData, NSString};

    use super::*;

    /// Replace the clipboard with these representations of one piece of content, for example
    /// the app's own format plus `public.utf8-plain-text` for other apps.
    pub fn set_custom(items: &[(&str, &[u8])]) -> Result<()> {
        check(items)?;
        let pb = NSPasteboard::generalPasteboard();
        pb.clearContents();
        for (ty, bytes) in items {
            let data = NSData::with_bytes(bytes);
            if !pb.setData_forType(Some(&data), &NSString::from_str(ty)) {
                return Err(NativeError::Os(format!("the pasteboard refused type `{ty}`")));
            }
        }
        Ok(())
    }

    /// The clipboard's data of type `ty`, `None` when it holds none.
    pub fn custom(ty: &str) -> Result<Option<Vec<u8>>> {
        if !valid_type(ty) {
            return Err(NativeError::Invalid(format!("`{}` is not a pasteboard type", ty.chars().take(64).collect::<String>())));
        }
        let pb = NSPasteboard::generalPasteboard();
        let data = pb.dataForType(&NSString::from_str(ty));
        let Some(data) = data else { return Ok(None) };
        if data.len() > MAX_CUSTOM_BYTES {
            return Err(NativeError::Os("the clipboard data is too large".into()));
        }
        Ok(Some(data.to_vec()))
    }

    /// The type names the clipboard currently holds.
    pub fn types() -> Result<Vec<String>> {
        let pb = NSPasteboard::generalPasteboard();
        Ok(pb.types().map(|a| a.iter().take(256).map(|t| t.to_string()).collect()).unwrap_or_default())
    }
}

#[cfg(not(target_os = "macos"))]
mod custom {
    use super::*;

    pub fn set_custom(items: &[(&str, &[u8])]) -> Result<()> {
        check(items)?;
        Err(NativeError::Unsupported)
    }
    pub fn custom(_: &str) -> Result<Option<Vec<u8>>> {
        Err(NativeError::Unsupported)
    }
    pub fn types() -> Result<Vec<String>> {
        Err(NativeError::Unsupported)
    }
}

/// Custom types: `set_custom(&[("com.example.clip", bytes), ("public.utf8-plain-text", text)])`,
/// `custom("com.example.clip")`, `types()`. macOS only for now; elsewhere `Unsupported`.
pub use custom::{custom, set_custom, types};

fn check(items: &[(&str, &[u8])]) -> Result<()> {
    if items.is_empty() || items.len() > 16 {
        return Err(NativeError::Invalid("copy between 1 and 16 representations".into()));
    }
    for (ty, bytes) in items {
        if !valid_type(ty) {
            return Err(NativeError::Invalid(format!("`{}` is not a pasteboard type", ty.chars().take(64).collect::<String>())));
        }
        if bytes.len() > MAX_CUSTOM_BYTES {
            return Err(NativeError::Invalid(format!("`{ty}` is larger than {MAX_CUSTOM_BYTES} bytes")));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn images_are_checked_before_the_os_sees_them() {
        assert!(Image { width: 2, height: 2, rgba: vec![0; 16] }.validate().is_ok());
        assert!(Image { width: 2, height: 2, rgba: vec![0; 15] }.validate().is_err());
        assert!(Image { width: 0, height: 2, rgba: vec![] }.validate().is_err());
        assert!(Image { width: u32::MAX, height: u32::MAX, rgba: vec![] }.validate().is_err());
        assert!(set_image(&Image { width: 3, height: 1, rgba: vec![1, 2] }).is_err());
    }

    #[test]
    fn custom_types_are_validated() {
        assert!(valid_type("com.example.app.clip"));
        assert!(valid_type("application/x-example+json"));
        for bad in ["", "a b", "a\nb", "ü", &"x".repeat(129)] {
            assert!(!valid_type(bad), "{bad:?}");
        }
        assert!(set_custom(&[]).is_err());
        assert!(matches!(set_custom(&[("bad type", b"x")]), Err(NativeError::Invalid(_))));
        assert!(matches!(custom("bad type"), Err(NativeError::Invalid(_)) | Err(NativeError::Unsupported)));
    }

    /// Writes to the real clipboard: `cargo test -p ashlar-native clipboard -- --ignored`.
    #[test]
    #[ignore = "replaces the user's clipboard"]
    fn real_clipboard_round_trip() {
        set_text("ashlar clipboard test").unwrap();
        assert_eq!(text().unwrap().as_deref(), Some("ashlar clipboard test"));
        let img = Image { width: 2, height: 1, rgba: vec![255, 0, 0, 255, 0, 0, 255, 255] };
        set_image(&img).unwrap();
        assert_eq!(image().unwrap().map(|i| (i.width, i.height)), Some((2, 1)));
        if cfg!(target_os = "macos") {
            set_custom(&[("com.example.ashlar.test", b"\x00\x01binary"), ("public.utf8-plain-text", b"fallback")]).unwrap();
            assert_eq!(custom("com.example.ashlar.test").unwrap().as_deref(), Some(&b"\x00\x01binary"[..]));
            assert_eq!(text().unwrap().as_deref(), Some("fallback"));
            assert!(types().unwrap().iter().any(|t| t == "com.example.ashlar.test"));
        }
        clear().unwrap();
    }
}
