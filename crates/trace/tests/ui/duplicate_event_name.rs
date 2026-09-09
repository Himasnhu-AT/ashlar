ashlar_trace::events! {
    pub enum Category { File = "file", Edit = "edit" }
    pub enum AppEvent {
        FileOpen = "file.open" in File,
        Undo = "edit.undo" in Edit,
        OpenAgain = "file.open" in Edit,
    }
}

fn main() {}
