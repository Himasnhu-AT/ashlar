//! Drag files, an image or text out of the window into other apps (Finder, a mail, a chat).
//!
//! Start it from the frame where the pointer drags an item: [`start`] hands the drag to the OS,
//! which tracks the pointer from there; `on_end` gets the [`Outcome`] when the drop lands or is
//! cancelled. macOS only for now (an `NSDraggingSession`); elsewhere [`NativeError::Unsupported`].

use std::path::PathBuf;

use crate::{NativeError, Result};

/// Most files in one drag.
pub const MAX_FILES: usize = 1000;
/// Largest PNG, in bytes.
pub const MAX_PNG: usize = 64 * 1024 * 1024;
/// Longest text, in bytes.
pub const MAX_TEXT: usize = 16 * 1024 * 1024;

/// What is dragged: any of files, a PNG image and text (a target takes the kind it understands).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DragItem {
    /// Absolute paths of existing files or folders.
    pub files: Vec<PathBuf>,
    /// An encoded PNG.
    pub png: Option<Vec<u8>>,
    pub text: Option<String>,
}

impl DragItem {
    pub fn files(files: impl IntoIterator<Item = impl Into<PathBuf>>) -> Self {
        Self { files: files.into_iter().map(Into::into).collect(), ..Self::default() }
    }

    pub fn png(bytes: Vec<u8>) -> Self {
        Self { png: Some(bytes), ..Self::default() }
    }

    pub fn text(text: impl Into<String>) -> Self {
        Self { text: Some(text.into()), ..Self::default() }
    }

    /// Something to drag, within the limits; files absolute and present.
    pub fn validate(&self) -> Result<()> {
        if self.files.is_empty() && self.png.is_none() && self.text.is_none() {
            return Err(NativeError::Invalid("nothing to drag".into()));
        }
        if self.files.len() > MAX_FILES {
            return Err(NativeError::Invalid(format!("at most {MAX_FILES} files in one drag")));
        }
        if let Some(f) = self.files.iter().find(|f| !f.is_absolute() || !f.exists()) {
            return Err(NativeError::Invalid(format!("`{}` is not an existing absolute path", f.display())));
        }
        match &self.png {
            Some(p) if p.len() > MAX_PNG || !p.starts_with(b"\x89PNG\r\n\x1a\n") => {
                return Err(NativeError::Invalid("the image is not a PNG under the size limit".into()));
            }
            _ => {}
        }
        if self.text.as_ref().is_some_and(|t| t.len() > MAX_TEXT) {
            return Err(NativeError::Invalid("the text is too long to drag".into()));
        }
        Ok(())
    }
}

/// How a drag ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// A target took it (copied it).
    Dropped,
    /// Dropped nowhere, or cancelled with Esc.
    Cancelled,
}

/// Begin dragging `item` out of `window`, from the pointer's position. Main thread, while the
/// mouse button is down. `on_end` runs on the main thread once the drag is over.
pub fn start(window: &impl raw_window_handle::HasWindowHandle, item: DragItem, on_end: impl FnOnce(Outcome) + 'static) -> Result<()> {
    item.validate()?;
    let raw = window.window_handle().map_err(|e| NativeError::Os(crate::os_text(e)))?.as_raw();
    imp::start(raw, &item, Box::new(on_end))
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod imp {
    use std::cell::RefCell;

    use objc2::rc::Retained;
    use objc2::runtime::{NSObject, NSObjectProtocol, ProtocolObject};
    use objc2::{AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
    use objc2_app_kit::{
        NSDragOperation, NSDraggingContext, NSDraggingItem, NSDraggingSession, NSDraggingSource, NSEvent, NSEventModifierFlags, NSEventType,
        NSPasteboardItem, NSPasteboardTypePNG, NSPasteboardWriting, NSView,
    };
    use objc2_foundation::{NSArray, NSData, NSPoint, NSRect, NSSize, NSString, NSURL};

    use super::*;

    type OnEnd = Box<dyn FnOnce(Outcome)>;

    pub struct Ivars {
        on_end: RefCell<Option<OnEnd>>,
    }

    define_class!(
        // SAFETY: NSObject has no subclassing requirements; the class adds no `dealloc`.
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "AshlarDragOutSource"]
        #[ivars = Ivars]
        struct Source;

        unsafe impl NSObjectProtocol for Source {}

        // SAFETY: the methods match NSDraggingSource's signatures.
        unsafe impl NSDraggingSource for Source {
            #[unsafe(method(draggingSession:sourceOperationMaskForDraggingContext:))]
            fn operation_mask(&self, _session: &NSDraggingSession, _context: NSDraggingContext) -> NSDragOperation {
                NSDragOperation::Copy
            }

            #[unsafe(method(draggingSession:endedAtPoint:operation:))]
            fn ended(&self, _session: &NSDraggingSession, _at: NSPoint, operation: NSDragOperation) {
                let outcome = if operation == NSDragOperation::None { Outcome::Cancelled } else { Outcome::Dropped };
                if let Some(f) = self.ivars().on_end.borrow_mut().take() {
                    // never unwind into AppKit
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(outcome)));
                }
                SOURCE.with(|s| s.borrow_mut().take());
            }
        }
    );

    thread_local! {
        /// The live drag's source, kept until the drag ends.
        static SOURCE: RefCell<Option<Retained<Source>>> = const { RefCell::new(None) };
    }

    fn os(what: &str) -> NativeError {
        NativeError::Os(what.into())
    }

    pub fn start(raw: raw_window_handle::RawWindowHandle, item: &DragItem, on_end: OnEnd) -> Result<()> {
        let mtm = MainThreadMarker::new().ok_or_else(|| os("start a drag on the main thread"))?;
        let raw_window_handle::RawWindowHandle::AppKit(h) = raw else { return Err(NativeError::Unsupported) };
        // SAFETY: `ns_view` comes from a live window handle borrowed by the caller, so it is a
        // valid `NSView` on this (the main) thread; retaining it keeps it valid here.
        let view: Retained<NSView> = unsafe { Retained::retain(h.ns_view.as_ptr().cast::<NSView>()) }.ok_or_else(|| os("the window has no view"))?;
        let window = view.window().ok_or_else(|| os("the view is not in a window"))?;
        let at_window = window.mouseLocationOutsideOfEventStream();
        let at = view.convertPoint_fromView(at_window, None);
        let frame = NSRect::new(NSPoint::new(at.x - 16.0, at.y - 16.0), NSSize::new(32.0, 32.0));

        let mut writers: Vec<Retained<ProtocolObject<dyn NSPasteboardWriting>>> = vec![];
        for f in &item.files {
            let url = NSURL::from_file_path(f).ok_or_else(|| NativeError::Invalid(format!("`{}` is not a file path", f.display())))?;
            writers.push(ProtocolObject::from_retained(url));
        }
        if let Some(png) = &item.png {
            let p = NSPasteboardItem::new();
            // SAFETY: the type constant is AppKit's.
            let ok = p.setData_forType(&NSData::with_bytes(png), unsafe { NSPasteboardTypePNG });
            if ok {
                writers.push(ProtocolObject::from_retained(p));
            }
        }
        if let Some(t) = &item.text {
            writers.push(ProtocolObject::from_retained(NSString::from_str(t)));
        }
        let items: Vec<Retained<NSDraggingItem>> = writers
            .iter()
            .map(|w| {
                let d = NSDraggingItem::initWithPasteboardWriter(NSDraggingItem::alloc(), w);
                // SAFETY: a frame in the view's coordinates and no image (nil contents is allowed).
                unsafe { d.setDraggingFrame_contents(frame, None) };
                d
            })
            .collect();
        if items.is_empty() {
            return Err(NativeError::Invalid("nothing the OS can drag".into()));
        }

        // A drag event at the pointer: the session takes its start point and window from it.
        let event = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
            NSEventType::LeftMouseDragged,
            at_window,
            NSEventModifierFlags::empty(),
            0.0,
            window.windowNumber(),
            None,
            0,
            1,
            1.0,
        )
        .ok_or_else(|| os("could not make the drag event"))?;

        let source = Source::alloc(mtm).set_ivars(Ivars { on_end: RefCell::new(Some(on_end)) });
        // SAFETY: plain `init` of an NSObject subclass whose ivars are set.
        let source: Retained<Source> = unsafe { msg_send![super(source), init] };
        let _session =
            view.beginDraggingSessionWithItems_event_source(&NSArray::from_retained_slice(&items), &event, ProtocolObject::from_ref(&*source));
        SOURCE.with(|s| *s.borrow_mut() = Some(source));
        Ok(())
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::*;

    pub fn start(_raw: raw_window_handle::RawWindowHandle, _item: &DragItem, _on_end: Box<dyn FnOnce(Outcome)>) -> Result<()> {
        Err(NativeError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn items_are_checked_before_the_os_sees_them() {
        assert!(DragItem::default().validate().is_err(), "nothing to drag");
        assert!(DragItem::text("hello").validate().is_ok());
        assert!(DragItem::files(["relative.txt"]).validate().is_err());
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.txt");
        std::fs::write(&f, "a").unwrap();
        assert!(DragItem::files([&f]).validate().is_ok());
        assert!(DragItem::files([dir.path().join("missing")]).validate().is_err());
        assert!(DragItem::png(b"not a png".to_vec()).validate().is_err());
        assert!(DragItem::png(b"\x89PNG\r\n\x1a\nrest".to_vec()).validate().is_ok());
        assert!(DragItem::files(vec![f.clone(); MAX_FILES + 1]).validate().is_err());
    }
}
