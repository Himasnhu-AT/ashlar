//! Documents: Arc snapshots, revisions, undo history, versioned formats with migrations, autosave.
//!
//! - [`Store`]: the document as an `Arc<D>`, edited copy-on-write, with a [`Revision`] per state,
//!   undo/redo, merged gestures, compound edits ([`Transaction`]) and dirty tracking.
//! - [`History`]: the undo stacks on their own (labels, extra state, merge keys, step and byte
//!   limits, optional branches).
//! - [`Tracked`] and [`Memo`]: per-part revisions and small revision-keyed caches for views.
//! - [`SnapshotCell`]: the latest snapshot for real-time and worker threads, lock-free.
//! - [`Format`]: the file envelope, migrations, refusal of newer and foreign files, durable saves.
//! - [`Autosave`]: an autosave ring with crash detection and [`RecoveryCandidate`]s.
//!
//! Headless and wasm32-clean (the background autosave writer is native only).
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod autosave;
pub mod format;
pub mod history;
pub mod revision;
pub mod snapshot;
pub mod store;

#[cfg(not(target_arch = "wasm32"))]
pub use autosave::BackgroundWriter;
pub use autosave::{Autosave, AutosaveConfig, RecoveryCandidate, SessionMeta, SlotWrite};
pub use format::{Envelope, Format, FormatError, Loaded, Migration};
pub use history::{Branch, Entry, History};
pub use revision::{Memo, Revision, Tracked, Versioned};
pub use snapshot::{Published, SnapshotCell};
pub use store::{Store, Transaction};
