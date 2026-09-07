//! Open, save and folder pickers (rfd).
//!
//! Describe the dialog with [`FileDialog`], then call a blocking picker ([`FileDialog::open_file`],
//! …) or its async twin ([`FileDialog::open_file_async`], …). A dismissed dialog is
//! [`NativeError::Cancelled`].
//!
//! Threads: on macOS the blocking pickers must run on the main thread (or on another thread while
//! the main thread is running its event loop); the async pickers run anywhere and are the right
//! choice inside an event loop. On the web only [`FileDialog::open_bytes_async`] exists (browsers
//! give the page a file's contents, never its path); everything else is `Unsupported`.
//!
//! [`message`] shows a modal message box, for what must be said before (or without) a window:
//! a start-up failure, command-line output in an app that has no console.

use std::path::{Path, PathBuf};

use crate::{NativeError, Result};

/// Longest title or file name passed to the OS.
const MAX_TEXT: usize = 512;
/// At most this many filters.
const MAX_FILTERS: usize = 64;

/// One filter row: a name and the extensions it accepts (without dots: `["png", "jpg"]`).
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct Filter {
    pub name: String,
    pub extensions: Vec<String>,
}

/// What a dialog shows. Every field is optional.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct FileDialog {
    pub title: Option<String>,
    /// The folder the dialog opens in.
    pub directory: Option<PathBuf>,
    /// The suggested name (save dialogs).
    pub file_name: Option<String>,
    pub filters: Vec<Filter>,
}

fn clip(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).take(MAX_TEXT).collect()
}

/// An extension as rfd wants it: no leading dot, no separators, short.
fn clean_extension(e: &str) -> Option<String> {
    let e = e.trim().trim_start_matches("*.").trim_start_matches('.');
    let ok = !e.is_empty() && e.len() <= 32 && e.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-' || c == '*');
    ok.then(|| e.to_string())
}

impl FileDialog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    pub fn directory(mut self, dir: impl AsRef<Path>) -> Self {
        self.directory = Some(dir.as_ref().to_path_buf());
        self
    }

    pub fn file_name(mut self, name: impl Into<String>) -> Self {
        self.file_name = Some(name.into());
        self
    }

    /// Add a filter row (`filter("Images", &["png", "jpg"])`). Extensions may be written `png`,
    /// `.png` or `*.png`; malformed ones are dropped, and a row left without extensions is skipped.
    pub fn filter(mut self, name: impl Into<String>, extensions: &[&str]) -> Self {
        self.filters.push(Filter { name: name.into(), extensions: extensions.iter().map(|e| e.to_string()).collect() });
        self
    }

    /// The filters as they are given to the OS: cleaned, bounded, empty rows dropped.
    pub fn clean_filters(&self) -> Vec<(String, Vec<String>)> {
        self.filters
            .iter()
            .take(MAX_FILTERS)
            .filter_map(|f| {
                let exts: Vec<String> = f.extensions.iter().filter_map(|e| clean_extension(e)).take(64).collect();
                (!exts.is_empty()).then(|| (clip(&f.name), exts))
            })
            .collect()
    }

    #[cfg(not(target_arch = "wasm32"))]
    /// The folder to open in, only when it exists (a missing folder makes some dialogs fail).
    fn existing_directory(&self) -> Option<&Path> {
        self.directory.as_deref().filter(|d| d.is_dir())
    }
}

/// Longest message text passed to the OS.
const MAX_MESSAGE: usize = 8192;

/// How serious a [`message`] is; the OS picks the icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MessageLevel {
    #[default]
    Info,
    Warning,
    Error,
}

/// Message text as it is given to the OS: line breaks and tabs kept, other control characters
/// dropped, at most [`MAX_MESSAGE`] characters.
fn clip_message(s: &str) -> String {
    s.chars().filter(|c| !c.is_control() || *c == '\n' || *c == '\t').take(MAX_MESSAGE).collect()
}

/// Show a modal message box with one OK button and wait until it is dismissed. Main thread on
/// macOS. `Unsupported` on the web.
pub fn message(level: MessageLevel, title: &str, text: &str) -> Result<()> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let level = match level {
            MessageLevel::Info => rfd::MessageLevel::Info,
            MessageLevel::Warning => rfd::MessageLevel::Warning,
            MessageLevel::Error => rfd::MessageLevel::Error,
        };
        let (title, text) = (clip(title), clip_message(text));
        crate::guarded("the message box", || {
            rfd::MessageDialog::new().set_level(level).set_title(title).set_description(text).set_buttons(rfd::MessageButtons::Ok).show();
            Ok(())
        })
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (level, title, text);
        Err(NativeError::Unsupported)
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use super::*;

    fn sync(d: &FileDialog) -> rfd::FileDialog {
        let mut r = rfd::FileDialog::new();
        if let Some(t) = &d.title {
            r = r.set_title(clip(t));
        }
        if let Some(dir) = d.existing_directory() {
            r = r.set_directory(dir);
        }
        if let Some(n) = &d.file_name {
            r = r.set_file_name(clip(n));
        }
        for (name, exts) in d.clean_filters() {
            r = r.add_filter(name, &exts);
        }
        r
    }

    fn asynch(d: &FileDialog) -> rfd::AsyncFileDialog {
        let mut r = rfd::AsyncFileDialog::new();
        if let Some(t) = &d.title {
            r = r.set_title(clip(t));
        }
        if let Some(dir) = d.existing_directory() {
            r = r.set_directory(dir);
        }
        if let Some(n) = &d.file_name {
            r = r.set_file_name(clip(n));
        }
        for (name, exts) in d.clean_filters() {
            r = r.add_filter(name, &exts);
        }
        r
    }

    fn picked<T>(what: &str, f: impl FnOnce() -> Option<T>) -> Result<T> {
        crate::guarded(what, || f().ok_or(NativeError::Cancelled))
    }

    impl FileDialog {
        /// Ask for one existing file.
        pub fn open_file(&self) -> Result<PathBuf> {
            picked("the open dialog", || sync(self).pick_file())
        }

        /// Ask for one or more existing files.
        pub fn open_files(&self) -> Result<Vec<PathBuf>> {
            picked("the open dialog", || sync(self).pick_files()).and_then(non_empty)
        }

        /// Ask for a folder.
        pub fn pick_folder(&self) -> Result<PathBuf> {
            picked("the folder dialog", || sync(self).pick_folder())
        }

        /// Ask where to save. The OS has already asked about replacing an existing file.
        pub fn save_file(&self) -> Result<PathBuf> {
            picked("the save dialog", || sync(self).save_file())
        }

        pub async fn open_file_async(&self) -> Result<PathBuf> {
            asynch(self).pick_file().await.map(|h| h.path().to_path_buf()).ok_or(NativeError::Cancelled)
        }

        pub async fn open_files_async(&self) -> Result<Vec<PathBuf>> {
            let files = asynch(self).pick_files().await.ok_or(NativeError::Cancelled)?;
            non_empty(files.iter().map(|h| h.path().to_path_buf()).collect())
        }

        pub async fn pick_folder_async(&self) -> Result<PathBuf> {
            asynch(self).pick_folder().await.map(|h| h.path().to_path_buf()).ok_or(NativeError::Cancelled)
        }

        pub async fn save_file_async(&self) -> Result<PathBuf> {
            asynch(self).save_file().await.map(|h| h.path().to_path_buf()).ok_or(NativeError::Cancelled)
        }

        /// Ask for one file and read it: `(file name, contents)`. Works on every platform,
        /// including the web. Files larger than `max_bytes` are refused.
        pub async fn open_bytes_async(&self, max_bytes: u64) -> Result<(String, Vec<u8>)> {
            let h = asynch(self).pick_file().await.ok_or(NativeError::Cancelled)?;
            let path = h.path();
            let len = std::fs::metadata(path).map_err(|e| NativeError::Os(crate::os_text(e)))?.len();
            if len > max_bytes {
                return Err(NativeError::Invalid(format!("the file is larger than {max_bytes} bytes")));
            }
            let bytes = std::fs::read(path).map_err(|e| NativeError::Os(crate::os_text(e)))?;
            Ok((h.file_name(), bytes))
        }
    }

    fn non_empty<T>(v: Vec<T>) -> Result<Vec<T>> {
        if v.is_empty() { Err(NativeError::Cancelled) } else { Ok(v) }
    }
}

#[cfg(target_arch = "wasm32")]
mod web {
    use super::*;

    impl FileDialog {
        pub fn open_file(&self) -> Result<PathBuf> {
            Err(NativeError::Unsupported)
        }
        pub fn open_files(&self) -> Result<Vec<PathBuf>> {
            Err(NativeError::Unsupported)
        }
        pub fn pick_folder(&self) -> Result<PathBuf> {
            Err(NativeError::Unsupported)
        }
        pub fn save_file(&self) -> Result<PathBuf> {
            Err(NativeError::Unsupported)
        }
        pub async fn open_file_async(&self) -> Result<PathBuf> {
            Err(NativeError::Unsupported)
        }
        pub async fn open_files_async(&self) -> Result<Vec<PathBuf>> {
            Err(NativeError::Unsupported)
        }
        pub async fn pick_folder_async(&self) -> Result<PathBuf> {
            Err(NativeError::Unsupported)
        }
        pub async fn save_file_async(&self) -> Result<PathBuf> {
            Err(NativeError::Unsupported)
        }

        /// Ask for one file and read it: `(file name, contents)`. Files larger than `max_bytes`
        /// are refused (the browser has read them by then; the check bounds what the app keeps).
        pub async fn open_bytes_async(&self, max_bytes: u64) -> Result<(String, Vec<u8>)> {
            let mut r = rfd::AsyncFileDialog::new();
            if let Some(t) = &self.title {
                r = r.set_title(clip(t));
            }
            for (name, exts) in self.clean_filters() {
                r = r.add_filter(name, &exts);
            }
            let h = r.pick_file().await.ok_or(NativeError::Cancelled)?;
            let bytes = h.read().await;
            if bytes.len() as u64 > max_bytes {
                return Err(NativeError::Invalid(format!("the file is larger than {max_bytes} bytes")));
            }
            Ok((h.file_name(), bytes))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_are_cleaned_and_bounded() {
        let d = FileDialog::new()
            .filter("Images", &["png", ".jpg", "*.webp", "", "a/b", "../x"])
            .filter("Nothing valid", &["/", ""])
            .filter("All", &["*"]);
        assert_eq!(
            d.clean_filters(),
            vec![("Images".to_string(), vec!["png".to_string(), "jpg".into(), "webp".into()]), ("All".to_string(), vec!["*".to_string()])]
        );
        let many = (0..500).fold(FileDialog::new(), |d, i| d.filter(format!("f{i}"), &["txt"]));
        assert_eq!(many.clean_filters().len(), MAX_FILTERS);
    }

    #[test]
    fn message_text_keeps_its_lines_and_is_bounded() {
        assert_eq!(clip_message("usage:\n\t-h\u{0}\u{1b}[31m"), "usage:\n\t-h[31m");
        assert_eq!(clip_message(&"y".repeat(100_000)).chars().count(), MAX_MESSAGE);
        let level: MessageLevel = serde_json::from_str("\"error\"").unwrap_or_default();
        assert_eq!(level, MessageLevel::Error);
    }

    #[test]
    fn hostile_text_is_clipped() {
        let long = "x".repeat(10_000);
        assert_eq!(clip(&long).len(), MAX_TEXT);
        assert_eq!(clip("a\nb\u{0}c"), "abc");
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_missing_directory_is_not_passed_on() {
        let d = FileDialog::new().directory("/definitely/not/here/ashlar");
        assert!(d.existing_directory().is_none());
        let tmp = std::env::temp_dir();
        assert_eq!(FileDialog::new().directory(&tmp).existing_directory(), Some(tmp.as_path()));
    }

    #[test]
    fn the_description_round_trips_through_json() {
        let d = FileDialog::new().title("Export").file_name("clip.mp4").filter("Video", &["mp4"]);
        let v = serde_json::to_string(&d).unwrap_or_default();
        let back: FileDialog = serde_json::from_str(&v).unwrap_or_default();
        assert_eq!(back, d);
    }
}
