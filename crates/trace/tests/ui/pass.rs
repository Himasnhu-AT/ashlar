use ashlar_trace::{Event, EventCategory, Tracer};

ashlar_trace::events! {
    /// What a command belongs to.
    pub enum Category {
        File = "file",
        /// Editing commands.
        Edit = "edit",
        Tools = "tools.v2",
    }
    #[allow(dead_code)]
    pub enum AppEvent {
        FileOpen = "file.open" in File,
        /// The longest allowed name is 64 bytes.
        Longest = "a123456789_123456789_123456789_123456789_123456789_123456789_123" in File,
        Undo = "edit.undo" in Edit,
        UndoAll = "edit.undo_all" in Edit,
    }
}

mod nested {
    ashlar_trace::events! {
        pub(crate) enum Group { Only = "only" }
        pub(crate) enum Nothing {}
    }
}

fn main() {
    assert_eq!(Category::ALL.len(), 3);
    assert_eq!(AppEvent::ALL.len(), 4);
    assert_eq!(AppEvent::UndoAll.name(), "edit.undo_all");
    assert_eq!(AppEvent::UndoAll.category(), Category::Edit);
    assert_eq!(Category::Tools.name(), "tools.v2");
    assert_eq!(AppEvent::Longest.name().len(), 64);
    assert!(<nested::Nothing as Event>::ALL.is_empty());
    assert_eq!(nested::Group::Only.name(), "only");

    let copy = AppEvent::FileOpen;
    let set: std::collections::HashSet<AppEvent> = [copy, copy, AppEvent::Undo].into_iter().collect();
    assert_eq!(set.len(), 2);
    assert_eq!(format!("{copy:?}"), "FileOpen");

    // A declared event is what the public API takes.
    let tracer = Tracer::builder("key", "host", "1.0.0").catalog::<AppEvent>().listen(Category::File).listen_events(&[AppEvent::Undo]).init();
    tracer.track(AppEvent::FileOpen);
    tracer.track_id(Category::Tools, "tools.run");
}
