ashlar_trace::events! {
    pub enum Category { File = "file" }
    pub enum AppEvent {
        FileOpen = "file.open" in File,
        FileSave = "File.Save" in File,
    }
}

fn main() {}
