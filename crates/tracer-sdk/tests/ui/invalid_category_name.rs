tracer_sdk::events! {
    pub enum Category { File = "file", Edit = "edit menu" }
    pub enum AppEvent {
        FileOpen = "file.open" in File,
    }
}

fn main() {}
