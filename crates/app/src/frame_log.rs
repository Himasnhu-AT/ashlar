//! `ASHLAR_FRAME_LOG=FILE` (or [`Runtime::log_frames`](crate::Runtime::log_frames)): one JSON line
//! per egui pass, for profiling a real app run by run.
//!
//! ```json
//! {"frame":12,"passes":1,"t":0.204,"wall_ms":3.1,"cpu_ms":2.9,"phases":{"pass_ms":3.1,"ui_ms":2.4},
//!  "reasons":["animation"],"commands":[],"input":false,"dropped":0}
//! ```
//!
//! `frame` counts the runtime's frames; `passes` is the pass's number within its frame (2 or more
//! when egui ran the frame again). `t` is seconds since the first pass. `cpu_ms` is the UI thread's
//! CPU time (`null` where it cannot be read). `phases.ui_ms` is the runtime's own part (menus,
//! view, dialogs). `dropped` counts lines lost because the writer fell behind.
//!
//! Lines go through a bounded queue to a writer thread, never touching the disk on the UI thread.
//! The file stops at [`MAX_BYTES`] with a last line `{"end":"size limit reached",…}`. Not available
//! on the web.

use std::io::{self, Write};
use std::path::Path;
use std::sync::mpsc::{Receiver, SyncSender, TrySendError};

use serde_json::json;

use crate::perf_warn::FrameSample;

/// The largest file the log writes.
pub const MAX_BYTES: u64 = 64 * 1024 * 1024;
/// Lines waiting for the writer; more are dropped (and counted).
const QUEUE: usize = 4096;
/// Room kept for the last line.
const END_RESERVE: u64 = 256;

/// The sending end, kept by the frame probe.
pub(crate) struct FrameLog {
    tx: Option<SyncSender<String>>,
    dropped: u64,
}

impl FrameLog {
    /// Create (truncate) `path` and start its writer thread.
    pub(crate) fn create(path: &Path, max_bytes: u64) -> io::Result<FrameLog> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let file = std::fs::File::create(path)?;
            let (tx, rx) = std::sync::mpsc::sync_channel::<String>(QUEUE);
            std::thread::Builder::new().name("ashlar-frame-log".into()).spawn(move || {
                let run = || write_lines(io::BufWriter::new(file), &rx, max_bytes);
                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(run)) {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => log::warn!("frame log stopped: {e}"),
                    Err(_) => log::warn!("frame log writer panicked"),
                }
            })?;
            Ok(FrameLog { tx: Some(tx), dropped: 0 })
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = (path, max_bytes);
            Err(io::Error::new(io::ErrorKind::Unsupported, "no frame log on the web"))
        }
    }

    /// Queue the line for one pass.
    pub(crate) fn frame(&mut self, s: &FrameSample) {
        let Some(tx) = &self.tx else { return };
        match tx.try_send(line(s, self.dropped)) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => self.dropped = self.dropped.saturating_add(1),
            // the writer stopped (size limit, disk error): stop building lines
            Err(TrySendError::Disconnected(_)) => self.tx = None,
        }
    }
}

fn round(ms: f64) -> f64 {
    (ms * 1000.0).round() / 1000.0
}

fn ms(d: std::time::Duration) -> f64 {
    round(d.as_secs_f64() * 1000.0)
}

/// One pass as a JSON line (no newline).
pub(crate) fn line(s: &FrameSample, dropped: u64) -> String {
    json!({
        "frame": s.frame,
        "passes": s.pass,
        "t": (s.t * 1e6).round() / 1e6,
        "wall_ms": ms(s.wall),
        "cpu_ms": s.cpu.map(ms),
        "phases": {"pass_ms": ms(s.wall), "ui_ms": s.ui.map(ms)},
        "reasons": s.reasons,
        "commands": s.commands,
        "input": s.input,
        "dropped": dropped,
    })
    .to_string()
}

/// Write lines as they come, flushing whenever the queue runs dry, until the senders are gone or
/// the file would pass `max_bytes`.
fn write_lines(mut w: impl Write, rx: &Receiver<String>, max_bytes: u64) -> io::Result<()> {
    let mut bytes = 0u64;
    let room = max_bytes.saturating_sub(END_RESERVE);
    while let Ok(first) = rx.recv() {
        let mut next = Some(first);
        while let Some(line) = next {
            let len = u64::try_from(line.len()).unwrap_or(u64::MAX).saturating_add(1);
            if bytes.saturating_add(len) > room {
                writeln!(w, "{}", json!({"end": "size limit reached", "max_bytes": max_bytes, "bytes": bytes}))?;
                return w.flush();
            }
            w.write_all(line.as_bytes())?;
            w.write_all(b"\n")?;
            bytes = bytes.saturating_add(len);
            next = rx.try_recv().ok();
        }
        w.flush()?;
    }
    w.flush()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn sample() -> FrameSample {
        FrameSample {
            frame: 3,
            t: 0.5,
            wall: Duration::from_micros(2500),
            cpu: Some(Duration::from_micros(2000)),
            ui: None,
            reasons: vec!["timer".into()],
            commands: vec!["app.undo"],
            pass: 1,
            ..FrameSample::default()
        }
    }

    #[test]
    fn a_line_has_the_schema() {
        let v: serde_json::Value = serde_json::from_str(&line(&sample(), 2)).unwrap();
        assert_eq!(v["frame"], 3);
        assert_eq!(v["wall_ms"], 2.5);
        assert_eq!(v["cpu_ms"], 2.0);
        assert_eq!(v["phases"]["ui_ms"], serde_json::Value::Null);
        assert_eq!(v["reasons"], json!(["timer"]));
        assert_eq!(v["commands"], json!(["app.undo"]));
        assert_eq!((v["passes"].as_u64(), v["dropped"].as_u64()), (Some(1), Some(2)));
    }

    #[test]
    fn the_file_stops_at_its_size_limit_with_a_last_line() {
        let (tx, rx) = std::sync::mpsc::sync_channel(QUEUE);
        let one = line(&sample(), 0);
        for _ in 0..100 {
            tx.send(one.clone()).unwrap();
        }
        drop(tx);
        let mut out = Vec::new();
        let max = END_RESERVE + 10 * (one.len() as u64 + 1);
        write_lines(&mut out, &rx, max).unwrap();
        let text = String::from_utf8(out).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 11, "10 lines fit, then the end line");
        assert!(lines[10].contains("size limit reached"), "{}", lines[10]);
        assert!(text.len() as u64 <= max);
        // the writer has hung up: the sender stops
        let mut log = FrameLog { tx: None, dropped: 0 };
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        drop(rx);
        log.tx = Some(tx);
        log.frame(&sample());
        assert!(log.tx.is_none());
    }

    #[test]
    fn a_full_queue_counts_dropped_lines() {
        let (tx, _rx) = std::sync::mpsc::sync_channel(1);
        let mut log = FrameLog { tx: Some(tx), dropped: 0 };
        log.frame(&sample());
        log.frame(&sample());
        log.frame(&sample());
        assert_eq!(log.dropped, 2);
    }
}
