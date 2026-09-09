//! The host-declared event catalog.
//!
//! A host lists its categories and events once with [`events!`](crate::events). Names are checked at
//! build time, and [`crate::Tracer::track`] only accepts a declared event, so a misspelled or undeclared
//! event does not compile.

/// A group of events the host can listen to as a whole.
pub trait EventCategory: Copy + 'static {
    /// Every category, in declaration order.
    const ALL: &'static [Self];

    /// Every declared event with the category it belongs to. [`events!`](crate::events) fills this in,
    /// which lets [`crate::Builder::listen`] register the catalog by itself. A hand-written
    /// implementation that leaves it empty must call [`crate::Builder::catalog`].
    const EVENTS: &'static [(&'static str, Self)] = &[];

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

/// Declares the host's event categories and events.
///
/// ```
/// tracer_sdk::events! {
///     pub enum Category { File = "file", Edit = "edit" }
///     pub enum AppEvent {
///         FileOpen = "file.open" in File,
///         Undo = "edit.undo" in Edit,
///     }
/// }
///
/// use tracer_sdk::{Event, EventCategory};
/// assert_eq!(AppEvent::FileOpen.name(), "file.open");
/// assert_eq!(AppEvent::Undo.category(), Category::Edit);
/// assert_eq!(Category::ALL.len(), 2);
/// ```
///
/// The first enum lists the categories, the second the events and the category each belongs to. Both
/// get `Copy`, `Clone`, `Debug`, `PartialEq`, `Eq` and `Hash`, and implement [`EventCategory`] and
/// [`Event`].
///
/// Names are lower-case `snake_case`, optionally dotted, start with a letter and are at most 64 bytes.
/// The build fails on a name that breaks the rule, on a name used twice within either enum, and on an
/// event placed in a category that was not declared.
#[macro_export]
macro_rules! events {
    (
        $(#[$category_meta:meta])*
        $category_vis:vis enum $category:ident {
            $( $(#[$category_variant_meta:meta])* $category_variant:ident = $category_name:literal ),+ $(,)?
        }
        $(#[$event_meta:meta])*
        $event_vis:vis enum $event:ident {
            $( $(#[$event_variant_meta:meta])* $event_variant:ident = $event_name:literal in $event_category:ident ),* $(,)?
        }
    ) => {
        $(#[$category_meta])*
        #[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
        $category_vis enum $category {
            $( $(#[$category_variant_meta])* $category_variant ),+
        }

        impl $crate::EventCategory for $category {
            const ALL: &'static [Self] = &[$( Self::$category_variant ),+];

            const EVENTS: &'static [(&'static str, Self)] = &[$( ($event_name, Self::$event_category) ),*];

            fn name(self) -> &'static str {
                match self {
                    $( Self::$category_variant => $category_name ),+
                }
            }
        }

        $(#[$event_meta])*
        #[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
        $event_vis enum $event {
            $( $(#[$event_variant_meta])* $event_variant ),*
        }

        impl $crate::Event for $event {
            type Category = $category;

            const ALL: &'static [Self] = &[$( Self::$event_variant ),*];

            fn name(self) -> &'static str {
                match self {
                    $( Self::$event_variant => $event_name, )*
                }
            }

            fn category(self) -> $category {
                match self {
                    $( Self::$event_variant => $category::$event_category, )*
                }
            }
        }

        const _: () = {
            $( ::core::assert!($crate::catalog::__name_is_valid($category_name), "{}", ::core::concat!("tracer: invalid category name ", ::core::stringify!($category_name), ": use lower-case snake_case, optionally dotted, starting with a letter, at most 64 bytes")); )+
            $( ::core::assert!($crate::catalog::__name_is_valid($event_name), "{}", ::core::concat!("tracer: invalid event name ", ::core::stringify!($event_name), ": use lower-case snake_case, optionally dotted, starting with a letter, at most 64 bytes")); )*
            ::core::assert!(!$crate::catalog::__has_duplicate(&[$( $category_name ),+]), "{}", ::core::concat!("tracer: a category name is used twice in ", ::core::stringify!($category)));
            ::core::assert!(!$crate::catalog::__has_duplicate(&[$( $event_name ),*]), "{}", ::core::concat!("tracer: an event name is used twice in ", ::core::stringify!($event)));
        };
    };
}

/// Build-time name check for [`events!`](crate::events). Not part of the public API.
#[doc(hidden)]
pub const fn __name_is_valid(name: &str) -> bool {
    tracer_proto::validate_name(name).is_ok()
}

/// Build-time duplicate check for [`events!`](crate::events). Not part of the public API.
#[doc(hidden)]
pub const fn __has_duplicate(names: &[&str]) -> bool {
    let mut first = 0;
    while first < names.len() {
        let mut second = first + 1;
        while second < names.len() {
            if tracer_proto::const_str_eq(names[first], names[second]) {
                return true;
            }
            second += 1;
        }
        first += 1;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::{__has_duplicate, __name_is_valid};
    use crate::{Event, EventCategory};

    crate::events! {
        /// Groups of commands.
        pub(crate) enum Category {
            /// File commands.
            File = "file",
            Edit = "edit",
            View = "view",
        }
        /// Everything the host can record.
        pub(crate) enum AppEvent {
            /// A document was opened.
            FileOpen = "file.open" in File,
            FileSave = "file.save" in File,
            Undo = "edit.undo" in Edit,
        }
    }

    mod empty {
        crate::events! {
            pub(crate) enum Category { Command = "command" }
            pub(crate) enum NoEvents {}
        }
    }

    #[test]
    fn generated_enums_describe_the_catalog() {
        assert_eq!(Category::ALL, &[Category::File, Category::Edit, Category::View]);
        assert_eq!(Category::ALL.iter().map(|category| category.name()).collect::<Vec<_>>(), vec!["file", "edit", "view"]);
        assert_eq!(AppEvent::ALL, &[AppEvent::FileOpen, AppEvent::FileSave, AppEvent::Undo]);
        assert_eq!(Category::EVENTS, &[("file.open", Category::File), ("file.save", Category::File), ("edit.undo", Category::Edit)]);
        assert!(empty::Category::EVENTS.is_empty());
        assert_eq!(AppEvent::FileSave.name(), "file.save");
        assert_eq!(AppEvent::FileSave.category(), Category::File);
        assert_eq!(AppEvent::Undo.category().name(), "edit");
        assert_eq!(format!("{:?}", AppEvent::Undo), "Undo");
        let set: std::collections::HashSet<AppEvent> = AppEvent::ALL.iter().copied().collect();
        assert_eq!(set.len(), 3);
    }

    #[test]
    fn an_event_enum_may_be_empty() {
        assert!(empty::NoEvents::ALL.is_empty());
        assert_eq!(empty::Category::Command.name(), "command");
    }

    #[test]
    fn build_time_checks() {
        assert!(__name_is_valid("file.open"));
        assert!(!__name_is_valid("File.Open"));
        assert!(!__name_is_valid(""));
        assert!(!__has_duplicate(&[]));
        assert!(!__has_duplicate(&["a", "b", "c"]));
        assert!(__has_duplicate(&["a", "b", "a"]));
        assert!(__has_duplicate(&["a", "b", "b"]));
    }
}
