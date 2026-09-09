ashlar_trace::events! {
    pub enum Category { File = "file" }
    pub enum AppEvent {
        FileOpen = "file.open" in File,
    }
}

fn main() {
    let tracer = ashlar_trace::Tracer::disabled();
    tracer.track(AppEvent::FileClose);
    tracer.track("file.close");
}
