tracer_sdk::events! {
    pub enum Category { File = "file" }
    pub enum AppEvent {
        FileOpen = "file.open" in File,
    }
}

fn main() {
    let tracer = tracer_sdk::Tracer::disabled();
    tracer.track(AppEvent::FileClose);
    tracer.track("file.close");
}
