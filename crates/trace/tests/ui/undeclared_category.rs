tracer_sdk::events! {
    pub enum Category { File = "file" }
    pub enum AppEvent {
        FileOpen = "file.open" in File,
        Undo = "edit.undo" in Edit,
    }
}

fn main() {}
