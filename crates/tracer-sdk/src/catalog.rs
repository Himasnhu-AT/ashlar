//! The host-declared event catalog.
//!
//! A host lists its categories and events once with [`events!`](crate::events). Names are checked at
//! build time, and [`crate::Tracer::track`] only accepts a declared event, so a misspelled or undeclared
//! event does not compile.

/// A group of events the host can listen to as a whole.
pub trait EventCategory: Copy + 'static {
    /// Every category, in declaration order.
    const ALL: &'static [Self];

    fn name(self) -> &'static str;
}

/// One declared usage event.
pub trait Event: Copy + 'static {
    type Category: EventCategory;

    /// Every event, in declaration order.
    const ALL: &'static [Self];

    fn name(self) -> &'static str;

    fn category(self) -> Self::Category;
}

/// A property value on an event. There is deliberately no free-form string: text must be `'static`,
/// which keeps user content such as file names out by construction.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Prop {
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(&'static str),
}
