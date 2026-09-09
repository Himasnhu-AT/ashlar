//! For apps' own tests (feature `testing`): a transport that keeps every envelope instead of
//! sending it, and a client started on it with short intervals.

use std::io::Read;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use ashlar_trace::proto::{Envelope, ItemBody};
use ashlar_trace::transport::{Reply, Request, Transport, TransportError};

use crate::{Target, Telemetry, TelemetryConfig};

/// Keeps every envelope the SDK sends, decoded. Clones share the record.
#[derive(Clone, Default)]
pub struct Recorder {
    sent: Arc<Mutex<Vec<Envelope>>>,
}

impl Transport for Recorder {
    fn send(&mut self, r: &Request<'_>) -> Result<Reply, TransportError> {
        let mut body = Vec::new();
        if r.gzip {
            flate2::read::GzDecoder::new(r.body).read_to_end(&mut body).map_err(|e| TransportError::Network(e.to_string()))?;
        } else {
            body = r.body.to_vec();
        }
        if let Ok(env) = serde_json::from_slice::<Envelope>(&body) {
            self.sent.lock().unwrap_or_else(PoisonError::into_inner).push(env);
        }
        Ok(Reply { status: 200, body: br#"{"accepted":1}"#.to_vec(), retry_after: None })
    }
}

impl Recorder {
    /// Every item sent so far, in order.
    pub fn items(&self) -> Vec<ItemBody> {
        self.sent.lock().unwrap_or_else(PoisonError::into_inner).iter().flat_map(|e| e.items.iter().map(|i| i.body.clone())).collect()
    }

    /// The item types sent so far (`session_start`, `event`, `perf`, `crash`…).
    pub fn types(&self) -> Vec<&'static str> {
        self.items().iter().map(ItemBody::type_name).collect()
    }

    /// The names of the events sent so far.
    pub fn events(&self) -> Vec<String> {
        self.items()
            .into_iter()
            .filter_map(|b| match b {
                ItemBody::Event(e) => Some(e.name),
                _ => None,
            })
            .collect()
    }
}

/// A client for `cfg` that sends to `recorder`, with intervals of milliseconds (a perf window is
/// 50 ms) and the hang threshold `hang`.
pub fn client(cfg: &TelemetryConfig, data_dir: &Path, recorder: &Recorder, hang: Duration) -> Telemetry {
    let target = Target { endpoint: "http://127.0.0.1:9".into(), key: "tk_test".into(), host: "127.0.0.1".into() };
    Telemetry::with_transport(cfg, &target, data_dir, recorder.clone(), |c| {
        c.tick_interval = Duration::from_millis(10);
        c.flush_interval = Duration::from_millis(20);
        c.heartbeat_interval = Duration::from_millis(50);
        c.perf_window = Duration::from_millis(50);
        c.retry_min = Duration::from_millis(10);
        c.retry_max = Duration::from_millis(20);
        c.hang_threshold = hang;
    })
}
