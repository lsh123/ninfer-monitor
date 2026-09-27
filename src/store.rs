// NInferMonitor
//
// This is free software; see the LICENSE.md file in the source distribution for precise wording.
//
// Copyright (C) 2026 Aleksey Sanin aleksey@aleksey.com. All Rights Reserved.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::BufRead;
use std::path::Path;

use crate::metrics::{
    LatencyPoint, MAX_SERIES_POINTS, RingBuffer, ThroughputSample, Window, WindowRecord,
};
use crate::parser::{
    ParsedEvent, RequestDoneEvent, RequestErrorEvent, RequestStartEvent, ServerStartEvent,
    ThroughputEvent,
};

/// In-flight requests are aged out after this long (FR-2.5).
pub const INFLIGHT_AGE_MS: u64 = 24 * 60 * 60 * 1_000;

/// The seen-request-id set (`seen_ids`) is bounded to this multiple of the
/// request-row cap so a long-running, busy server cannot grow it without bound
/// (a distinct `request_id` is observed on every request event). A small
/// multiple of the cap keeps recently evicted ids remembered — so a late event
/// for an evicted row is not double-counted — while capping memory at a few
/// thousand entries.
const SEEN_IDS_RETAIN_FACTOR: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestStatus {
    InFlight,
    Done,
    Error,
}

/// Lifecycle state of a single request, keyed by `request_id` (FR-2.5).
#[derive(Debug, Clone)]
pub struct RequestState {
    pub id: u64,
    pub status: RequestStatus,
    pub start: Option<RequestStartEvent>,
    pub done: Option<RequestDoneEvent>,
    pub error: Option<RequestErrorEvent>,
}

impl RequestState {
    pub fn start_time_ms(&self) -> Option<u64> {
        self.start.as_ref().and_then(|s| s.timestamp_unix_ms)
    }

    pub fn model(&self) -> Option<&str> {
        self.start
            .as_ref()
            .and_then(|s| s.request.model.as_deref())
            .or_else(|| self.done.as_ref().and_then(|d| d.request.model.as_deref()))
    }
}

/// In-memory state for the dashboard (FR-3/FR-4/§9).
///
/// All mutation happens on the parse thread; the UI reads snapshots (M5).
/// `Clone` backs the `StoreUpdate` snapshots: the clone is bounded by the
/// store's own caps (up to 1 000 request rows, 10 000 series points).
#[derive(Debug, Clone)]
pub struct Store {
    server: Option<ServerStartEvent>,
    server_start_count: usize,
    restarts: Vec<u64>,
    requests: HashMap<u64, RequestState>,
    request_order: VecDeque<u64>,
    seen_ids: HashSet<u64>,
    seen_order: VecDeque<u64>,
    max_requests: usize,
    dropped_requests: usize,
    throughput: RingBuffer<ThroughputSample>,
    latency: RingBuffer<LatencyPoint>,
    request_records: RingBuffer<WindowRecord>,
    window: Window,
    total_requests: usize,
    total_done: usize,
    total_error: usize,
    skipped_lines: usize,
    max_timestamp_ms: Option<u64>,
    max_schema_version: Option<u32>,
}

impl Store {
    /// A store with the default request-row cap (FR-5.5).
    pub fn new() -> Self {
        Self::with_max_requests(crate::config::DEFAULT_MAX_REQUESTS as usize)
    }

    /// A store that keeps at most `max_requests` request rows in memory
    /// (FR-5.5).
    pub fn with_max_requests(max_requests: usize) -> Self {
        Self {
            server: None,
            server_start_count: 0,
            restarts: Vec::new(),
            requests: HashMap::new(),
            request_order: VecDeque::new(),
            seen_ids: HashSet::new(),
            seen_order: VecDeque::new(),
            max_requests,
            dropped_requests: 0,
            throughput: RingBuffer::new(MAX_SERIES_POINTS),
            latency: RingBuffer::new(MAX_SERIES_POINTS),
            request_records: RingBuffer::new(MAX_SERIES_POINTS),
            window: Window::default(),
            total_requests: 0,
            total_done: 0,
            total_error: 0,
            skipped_lines: 0,
            max_timestamp_ms: None,
            max_schema_version: None,
        }
    }

    pub fn load_jsonl(path: impl AsRef<Path>) -> std::io::Result<Self> {
        let file = std::fs::File::open(path)?;
        let reader = std::io::BufReader::new(file);
        let mut store = Self::new();
        for line in reader.lines() {
            let line = line?;
            match crate::parser::parse_line(line.as_bytes()) {
                Ok(event) => store.apply(&event),
                Err(_) => store.note_skipped_line(),
            }
        }
        Ok(store)
    }

    /// Fold a parsed event into the store.
    pub fn apply(&mut self, event: &ParsedEvent) {
        let ts = event.timestamp_unix_ms();
        if let Some(v) = event.schema_version() {
            self.max_schema_version = Some(self.max_schema_version.map_or(v, |a| a.max(v)));
        }
        match event {
            ParsedEvent::ServerStart(e) => self.apply_server_start(e),
            ParsedEvent::RequestStart(e) => self.apply_request_start(e),
            ParsedEvent::RequestDone(e) => self.apply_request_done(e),
            ParsedEvent::Throughput(e) => self.apply_throughput(e),
            ParsedEvent::RequestError(e) => self.apply_request_error(e),
            ParsedEvent::Unknown { .. } => {}
        }
        if let Some(ts) = ts {
            self.max_timestamp_ms = Some(self.max_timestamp_ms.map_or(ts, |a| a.max(ts)));
            self.window.advance(ts);
        }
        self.age_out_inflight();
    }

    fn apply_server_start(&mut self, e: &ServerStartEvent) {
        self.server_start_count += 1;
        if self.server_start_count > 1 {
            if let Some(ts) = e.timestamp_unix_ms {
                self.restarts.push(ts);
            }
            self.reset_for_restart();
        }
        self.server = Some(e.clone());
    }

    /// Reset per-request and windowed state on a server restart (FR-6.2).
    ///
    /// Throughput history is intentionally preserved so time-series charts can
    /// show a continuous series across restarts.
    fn reset_for_restart(&mut self) {
        self.requests.clear();
        self.request_order.clear();
        self.seen_order.clear();
        self.seen_ids.clear();
        self.window.clear();
        self.total_requests = 0;
        self.total_done = 0;
        self.total_error = 0;
        self.dropped_requests = 0;
    }

    fn apply_request_start(&mut self, e: &RequestStartEvent) {
        let id = e.request.request_id.unwrap_or(0);
        match self.requests.get_mut(&id) {
            Some(state) if state.status == RequestStatus::InFlight => {
                state.start = Some(e.clone());
                self.request_order.retain(|&x| x != id);
                self.request_order.push_front(id);
            }
            Some(state) => {
                if state.start.is_none() {
                    state.start = Some(e.clone());
                }
            }
            None => {
                self.note_request_seen(id);
                self.requests.insert(
                    id,
                    RequestState {
                        id,
                        status: RequestStatus::InFlight,
                        start: Some(e.clone()),
                        done: None,
                        error: None,
                    },
                );
                self.request_order.push_front(id);
                self.evict_if_needed();
            }
        }
    }

    fn apply_request_done(&mut self, e: &RequestDoneEvent) {
        let id = e.request.request_id.unwrap_or(0);
        let first_close = match self.requests.get_mut(&id) {
            Some(state) => {
                let first = state.status == RequestStatus::InFlight;
                if first {
                    state.status = RequestStatus::Done;
                }
                state.done = Some(e.clone());
                first
            }
            None => {
                self.note_request_seen(id);
                self.requests.insert(
                    id,
                    RequestState {
                        id,
                        status: RequestStatus::Done,
                        start: None,
                        done: Some(e.clone()),
                        error: None,
                    },
                );
                self.request_order.push_front(id);
                self.evict_if_needed();
                true
            }
        };
        if first_close {
            self.total_done += 1;
            let record = WindowRecord::from_done(e);
            let ttft_ms = record.ttft.map(|t| t * 1000.0);
            let decode_latency_ms = record.decode_latency_per_token.map(|t| t * 1000.0);
            if ttft_ms.is_some() || decode_latency_ms.is_some() {
                self.latency.push(LatencyPoint {
                    timestamp_ms: record.ts,
                    ttft_ms,
                    decode_latency_ms,
                });
            }
            self.request_records.push(record.clone());
            self.window.add_request(record);
        }
    }

    fn apply_request_error(&mut self, e: &RequestErrorEvent) {
        let id = e.request.request_id.unwrap_or(0);
        let first_close = match self.requests.get_mut(&id) {
            Some(state) => {
                let first = state.status == RequestStatus::InFlight;
                if first {
                    state.status = RequestStatus::Error;
                }
                state.error = Some(e.clone());
                first
            }
            None => {
                self.note_request_seen(id);
                self.requests.insert(
                    id,
                    RequestState {
                        id,
                        status: RequestStatus::Error,
                        start: None,
                        done: None,
                        error: Some(e.clone()),
                    },
                );
                self.request_order.push_front(id);
                self.evict_if_needed();
                true
            }
        };
        if first_close {
            self.total_error += 1;
            if let Some(ts) = e.timestamp_unix_ms {
                self.window.add_error(ts);
            }
        }
    }

    fn apply_throughput(&mut self, e: &ThroughputEvent) {
        self.throughput.push(ThroughputSample::from_event(e));
    }

    /// Record a first-sighting of `id` for the unique request count.
    ///
    /// `seen_ids` is bounded to a small multiple of the request-row cap
    /// (floored at 1) so it cannot grow without bound on a long-running, busy
    /// server. The oldest ids are forgotten first (via `seen_order`), which
    /// keeps recently evicted ids remembered so a late event for them is not
    /// double-counted.
    fn note_request_seen(&mut self, id: u64) {
        if self.seen_ids.insert(id) {
            self.total_requests += 1;
            self.seen_order.push_back(id);
            let retain = self
                .max_requests
                .saturating_mul(SEEN_IDS_RETAIN_FACTOR)
                .max(1);
            if self.seen_order.len() > retain {
                while let Some(oldest) = self.seen_order.pop_front() {
                    self.seen_ids.remove(&oldest);
                    if self.seen_ids.len() <= retain {
                        break;
                    }
                }
            }
        }
    }

    /// Keep the request list bounded to the configured cap by dropping the
    /// oldest rows, including in-flight rows if necessary (FR-5.5).
    fn evict_if_needed(&mut self) {
        while self.request_order.len() > self.max_requests {
            let Some(&id) = self.request_order.back() else {
                break;
            };
            self.request_order.pop_back();
            self.requests.remove(&id);
            self.dropped_requests += 1;
        }
    }

    /// Drop in-flight requests whose start is older than 24 h (FR-2.5).
    fn age_out_inflight(&mut self) {
        let Some(now) = self.max_timestamp_ms else {
            return;
        };
        let cutoff = now.saturating_sub(INFLIGHT_AGE_MS);
        // TODO(perf): full request scan per event plus per-ID `retain` can be
        // O(n^2) when many stale IDs appear; use a start-time queue/index.
        let stale: Vec<u64> = self
            .requests
            .iter()
            .filter(|(_, s)| s.status == RequestStatus::InFlight)
            .filter(|(_, s)| s.start_time_ms().is_some_and(|t| t < cutoff))
            .map(|(id, _)| *id)
            .collect();
        for id in stale {
            self.requests.remove(&id);
            self.request_order.retain(|&x| x != id);
        }
    }

    pub fn server(&self) -> Option<&ServerStartEvent> {
        self.server.as_ref()
    }

    pub fn server_start_count(&self) -> usize {
        self.server_start_count
    }

    /// Timestamps of `server_start` events after the first (restart markers,
    /// FR-6.2).
    pub fn restarts(&self) -> &[u64] {
        &self.restarts
    }

    /// Number of open (in-flight) requests (FR-3.1).
    pub fn active_requests(&self) -> usize {
        self.requests
            .values()
            .filter(|s| s.status == RequestStatus::InFlight)
            .count()
    }

    pub fn total_requests(&self) -> usize {
        self.total_requests
    }

    pub fn total_done(&self) -> usize {
        self.total_done
    }

    pub fn total_error(&self) -> usize {
        self.total_error
    }

    pub fn total_closed(&self) -> usize {
        self.total_done + self.total_error
    }

    pub fn request(&self, id: u64) -> Option<&RequestState> {
        self.requests.get(&id)
    }

    /// Request rows, newest first (FR-5.1).
    pub fn request_list(&self) -> impl Iterator<Item = &RequestState> + '_ {
        self.request_order
            .iter()
            .filter_map(|id| self.requests.get(id))
    }

    pub fn request_count(&self) -> usize {
        self.request_order.len()
    }

    pub fn dropped_requests(&self) -> usize {
        self.dropped_requests
    }

    pub fn throughput(&self) -> &RingBuffer<ThroughputSample> {
        &self.throughput
    }

    pub fn latest_throughput(&self) -> Option<&ThroughputSample> {
        self.throughput.last()
    }

    /// Latency points (TTFT + per-token decode latency) for the Latency
    /// widget, oldest first.
    pub fn latency_points(&self) -> &RingBuffer<LatencyPoint> {
        &self.latency
    }

    /// Completed-request records for the Cache widget's windowed rates,
    /// oldest first.
    pub fn request_records(&self) -> &RingBuffer<WindowRecord> {
        &self.request_records
    }

    pub fn window(&self) -> &Window {
        &self.window
    }

    pub fn skipped_lines(&self) -> usize {
        self.skipped_lines
    }

    pub fn note_skipped_line(&mut self) {
        self.skipped_lines += 1;
    }

    pub fn max_timestamp_ms(&self) -> Option<u64> {
        self.max_timestamp_ms
    }

    /// Highest `schema_version` seen in the log (FR-2.4).
    pub fn max_schema_version(&self) -> Option<u32> {
        self.max_schema_version
    }

    /// The configured cap on request rows kept in memory (FR-5.5).
    pub fn max_requests(&self) -> usize {
        self.max_requests
    }
}

impl Default for Store {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse_line;

    fn throughput_line(ts: u64) -> ParsedEvent {
        let raw = format!(
            r#"{{"artifact_type":"ninfer_serve_request_log","schema_version":20,"server_instance_id":"s","timestamp_unix_ms":{ts},"event":"throughput","throughput_tokens_per_second":{{"decode":1.5,"prefill":2.5}},"scheduler":{{"running":1,"prefilling":0,"waiting":2}},"context_cache":{{"occupancy":{{"device_main_kv_pages":10,"device_backend_kv_pages":11,"host_kv_bytes":100}}}}}}"#
        );
        parse_line(raw.as_bytes()).unwrap()
    }

    fn request_start_line(ts: u64, id: u64) -> ParsedEvent {
        let raw = format!(
            r#"{{"artifact_type":"ninfer_serve_request_log","schema_version":20,"server_instance_id":"s","timestamp_unix_ms":{ts},"event":"request_start","request":{{"request_id":{id},"model":"m"}}}}"#
        );
        parse_line(raw.as_bytes()).unwrap()
    }

    fn request_done_line(ts: u64, id: u64) -> ParsedEvent {
        let raw = format!(
            r#"{{"artifact_type":"ninfer_serve_request_log","schema_version":20,"server_instance_id":"s","timestamp_unix_ms":{ts},"event":"request_done","request":{{"request_id":{id}}},"result":{{"prompt_tokens":100,"completion_tokens":50,"prefix_cache_hit_tokens":25}},"timings_seconds":{{"ttft":0.5,"decode":1.0,"total":1.5}},"speculative":{{"drafted_tokens":10,"accepted_tokens":5}}}}"#
        );
        parse_line(raw.as_bytes()).unwrap()
    }

    fn request_error_line(ts: u64, id: u64) -> ParsedEvent {
        let raw = format!(
            r#"{{"artifact_type":"ninfer_serve_request_log","schema_version":20,"server_instance_id":"s","timestamp_unix_ms":{ts},"event":"request_error","request":{{"request_id":{id}}},"error":{{"message":"client disconnected"}}}}"#
        );
        parse_line(raw.as_bytes()).unwrap()
    }

    #[test]
    fn empty_store_has_no_data() {
        let store = Store::new();
        assert!(store.server().is_none());
        assert_eq!(store.active_requests(), 0);
        assert_eq!(store.total_done(), 0);
        assert_eq!(store.total_error(), 0);
        assert!(store.latest_throughput().is_none());
        assert!(store.window().avg_ttft().is_none());
        assert_eq!(store.request_count(), 0);
    }

    #[test]
    fn max_schema_version_tracks_highest_seen() {
        let mut store = Store::new();
        assert_eq!(store.max_schema_version(), None);
        store.apply(&throughput_line(1000));
        assert_eq!(store.max_schema_version(), Some(20));
        let raw = r#"{"artifact_type":"ninfer_serve_request_log","schema_version":21,"server_instance_id":"s","timestamp_unix_ms":2000,"event":"throughput","throughput_tokens_per_second":{"decode":1.0,"prefill":2.0}}"#;
        store.apply(&parse_line(raw.as_bytes()).unwrap());
        assert_eq!(store.max_schema_version(), Some(21));
        store.apply(&throughput_line(3000));
        assert_eq!(store.max_schema_version(), Some(21));
    }

    #[test]
    fn server_start_is_stored() {
        let mut store = Store::new();
        let raw = r#"{"artifact_type":"ninfer_serve_request_log","schema_version":20,"server_instance_id":"s","timestamp_unix_ms":1000,"event":"server_start","engine":{"kv_capacity":240000},"environment":{"gpu_name":"RTX 5090"}}"#;
        store.apply(&parse_line(raw.as_bytes()).unwrap());
        let server = store.server().expect("server info");
        assert_eq!(server.engine.kv_capacity, Some(240000));
        assert_eq!(server.environment.gpu_name.as_deref(), Some("RTX 5090"));
        assert_eq!(store.server_start_count(), 1);
        assert!(store.restarts().is_empty());
    }

    #[test]
    fn second_server_start_records_restart_and_resets() {
        let mut store = Store::new();
        let raw1 = r#"{"artifact_type":"ninfer_serve_request_log","schema_version":20,"server_instance_id":"s1","timestamp_unix_ms":1000,"event":"server_start"}"#;
        store.apply(&parse_line(raw1.as_bytes()).unwrap());
        store.apply(&request_done_line(2000, 1));
        assert_eq!(store.total_done(), 1);

        let raw2 = r#"{"artifact_type":"ninfer_serve_request_log","schema_version":20,"server_instance_id":"s2","timestamp_unix_ms":5000,"event":"server_start"}"#;
        store.apply(&parse_line(raw2.as_bytes()).unwrap());

        assert_eq!(store.server_start_count(), 2);
        assert_eq!(store.restarts(), &[5000]);
        assert_eq!(store.total_requests(), 0);
        assert_eq!(store.total_done(), 0);
        assert_eq!(store.request_count(), 0);
        assert_eq!(
            store.server().and_then(|s| s.server_instance_id.as_deref()),
            Some("s2")
        );
    }

    #[test]
    fn request_lifecycle_inflight_to_done() {
        let mut store = Store::new();
        store.apply(&request_start_line(1000, 1));
        assert_eq!(store.active_requests(), 1);
        let state = store.request(1).expect("request 1");
        assert_eq!(state.status, RequestStatus::InFlight);
        assert_eq!(state.model(), Some("m"));

        store.apply(&request_done_line(2000, 1));
        assert_eq!(store.active_requests(), 0);
        let state = store.request(1).expect("request 1");
        assert_eq!(state.status, RequestStatus::Done);
        assert_eq!(
            state.done.as_ref().and_then(|d| d.result.prompt_tokens),
            Some(100)
        );
        assert_eq!(store.total_done(), 1);
    }

    #[test]
    fn request_error_marks_error_status() {
        let mut store = Store::new();
        store.apply(&request_start_line(1000, 7));
        store.apply(&request_error_line(2000, 7));
        assert_eq!(store.active_requests(), 0);
        let state = store.request(7).expect("request 7");
        assert_eq!(state.status, RequestStatus::Error);
        assert_eq!(
            state
                .error
                .as_ref()
                .and_then(|e| e.error.message.as_deref()),
            Some("client disconnected")
        );
        assert_eq!(store.total_error(), 1);
    }

    #[test]
    fn request_done_without_start_is_recorded() {
        let mut store = Store::new();
        store.apply(&request_done_line(2000, 42));
        assert_eq!(store.total_done(), 1);
        let state = store.request(42).expect("request 42");
        assert_eq!(state.status, RequestStatus::Done);
        assert!(state.start.is_none());
    }

    #[test]
    fn request_list_is_newest_first() {
        let mut store = Store::new();
        for id in 1..=5 {
            store.apply(&request_start_line(id * 1000, id));
            store.apply(&request_done_line(id * 1000 + 500, id));
        }
        let ids: Vec<u64> = store.request_list().map(|s| s.id).collect();
        assert_eq!(ids, vec![5, 4, 3, 2, 1]);
    }

    #[test]
    fn new_store_uses_default_max_requests() {
        let store = Store::new();
        assert_eq!(
            store.max_requests(),
            crate::config::DEFAULT_MAX_REQUESTS as usize
        );
    }

    #[test]
    fn request_list_is_bounded_to_max_requests() {
        let cap = 100;
        let mut store = Store::with_max_requests(cap);
        let n = cap + 500;
        for id in 1..=n as u64 {
            store.apply(&request_start_line(id, id));
            store.apply(&request_done_line(id + 1, id));
        }
        assert_eq!(store.request_count(), cap);
        assert_eq!(store.dropped_requests(), 500);
        assert_eq!(store.total_done(), n);
        let ids: Vec<u64> = store.request_list().map(|s| s.id).collect();
        assert_eq!(ids.first().copied(), Some(n as u64));
        assert_eq!(ids.last().copied(), Some(n as u64 - cap as u64 + 1));
    }

    #[test]
    fn throughput_ring_buffer_is_bounded() {
        let mut store = Store::new();
        let n = MAX_SERIES_POINTS + 500;
        for ts in 0..n {
            store.apply(&throughput_line(ts as u64));
        }
        assert_eq!(store.throughput().len(), MAX_SERIES_POINTS);
        let latest = store.latest_throughput().expect("latest sample");
        assert_eq!(latest.timestamp_ms, (n - 1) as u64);
    }

    #[test]
    fn windowed_metrics_track_60s() {
        let mut store = Store::new();
        // Two completed requests 10 s apart, then a throughput event 100 s
        // later pushes both out of the window.
        store.apply(&request_done_line(1_000, 1));
        store.apply(&request_done_line(11_000, 2));
        assert_eq!(store.window().request_count(), 2);
        assert_eq!(store.window().avg_ttft(), Some(0.5));

        store.apply(&throughput_line(101_000));
        assert_eq!(store.window().request_count(), 0);
        assert_eq!(store.window().avg_ttft(), None);
    }

    #[test]
    fn window_error_count_and_total() {
        let mut store = Store::new();
        store.apply(&request_error_line(1_000, 1));
        store.apply(&request_error_line(2_000, 2));
        assert_eq!(store.total_error(), 2);
        assert_eq!(store.window().error_count(), 2);
        store.apply(&throughput_line(100_000));
        assert_eq!(store.total_error(), 2);
        assert_eq!(store.window().error_count(), 0);
    }

    #[test]
    fn skipped_lines_counter() {
        let mut store = Store::new();
        store.note_skipped_line();
        store.note_skipped_line();
        assert_eq!(store.skipped_lines(), 2);
    }

    #[test]
    fn inflight_requests_are_aged_out_after_24h() {
        let mut store = Store::new();
        store.apply(&request_start_line(1_000, 1));
        assert_eq!(store.active_requests(), 1);
        // A much later event ages out the stale in-flight request.
        store.apply(&throughput_line(1_000 + INFLIGHT_AGE_MS + 1));
        assert_eq!(store.active_requests(), 0);
        assert!(store.request(1).is_none());
    }

    #[test]
    fn recent_inflight_requests_are_not_aged_out() {
        let mut store = Store::new();
        store.apply(&request_start_line(1_000, 1));
        store.apply(&throughput_line(1_000 + INFLIGHT_AGE_MS / 2));
        assert_eq!(store.active_requests(), 1);
    }

    #[test]
    fn unknown_event_is_ignored() {
        let mut store = Store::new();
        let raw = r#"{"artifact_type":"ninfer_serve_request_log","schema_version":20,"server_instance_id":"s","timestamp_unix_ms":1000,"event":"future_event"}"#;
        store.apply(&parse_line(raw.as_bytes()).unwrap());
        assert_eq!(store.total_done(), 0);
        assert_eq!(store.max_timestamp_ms(), Some(1000));
    }

    #[test]
    fn duplicate_request_start_does_not_duplicate_row() {
        let mut store = Store::new();
        let start = request_start_line(1_000, 1);
        store.apply(&start);
        store.apply(&start);
        assert_eq!(store.request_count(), 1);
        assert_eq!(store.active_requests(), 1);
        let ids: Vec<u64> = store.request_list().map(|s| s.id).collect();
        assert_eq!(ids, vec![1]);
    }

    #[test]
    fn duplicate_request_done_is_counted_once() {
        let mut store = Store::new();
        store.apply(&request_start_line(1_000, 1));
        let done = request_done_line(2_000, 1);
        store.apply(&done);
        store.apply(&done);
        assert_eq!(store.total_done(), 1);
        assert_eq!(store.window().request_count(), 1);
        assert_eq!(store.window().avg_ttft(), Some(0.5));
    }

    #[test]
    fn duplicate_request_error_is_counted_once() {
        let mut store = Store::new();
        store.apply(&request_start_line(1_000, 1));
        let err = request_error_line(2_000, 1);
        store.apply(&err);
        store.apply(&err);
        assert_eq!(store.total_error(), 1);
        assert_eq!(store.window().error_count(), 1);
    }

    #[test]
    fn request_done_after_error_is_not_double_counted() {
        let mut store = Store::new();
        store.apply(&request_start_line(1_000, 1));
        store.apply(&request_error_line(2_000, 1));
        store.apply(&request_done_line(3_000, 1));
        // The request was already closed by the error; the late done must not
        // add a second count or a second window record.
        assert_eq!(store.total_error(), 1);
        assert_eq!(store.total_done(), 0);
        assert_eq!(store.total_closed(), 1);
        assert_eq!(store.window().request_count(), 0);
        assert_eq!(store.window().error_count(), 1);
        let state = store.request(1).expect("request 1");
        assert_eq!(state.status, RequestStatus::Error);
    }

    #[test]
    fn request_list_hard_cap_evicts_oldest_inflight() {
        let cap = 10;
        let mut store = Store::with_max_requests(cap);
        for id in 1..=cap as u64 {
            store.apply(&request_start_line(id, id));
        }
        assert_eq!(store.request_count(), cap);
        assert_eq!(store.active_requests(), cap);

        let next_id = cap as u64 + 1;
        store.apply(&request_start_line(next_id, next_id));
        assert_eq!(store.request_count(), cap);
        assert_eq!(store.active_requests(), cap);
        assert_eq!(store.dropped_requests(), 1);
        assert!(store.request(1).is_none());
        assert!(store.request(next_id).is_some());
    }

    #[test]
    fn request_start_after_done_preserves_done_state() {
        let mut store = Store::new();
        store.apply(&request_done_line(2_000, 1));
        store.apply(&request_start_line(1_000, 1));

        assert_eq!(store.total_requests(), 1);
        assert_eq!(store.total_done(), 1);
        assert_eq!(store.active_requests(), 0);
        let state = store.request(1).expect("request 1");
        assert_eq!(state.status, RequestStatus::Done);
        assert!(state.start.is_some());
        assert!(state.done.is_some());
    }

    #[test]
    fn request_start_after_error_preserves_error_state() {
        let mut store = Store::new();
        store.apply(&request_error_line(2_000, 1));
        store.apply(&request_start_line(1_000, 1));

        assert_eq!(store.total_requests(), 1);
        assert_eq!(store.total_error(), 1);
        assert_eq!(store.active_requests(), 0);
        let state = store.request(1).expect("request 1");
        assert_eq!(state.status, RequestStatus::Error);
        assert!(state.start.is_some());
        assert!(state.error.is_some());
    }

    #[test]
    fn total_requests_counts_unique_observed_requests() {
        let mut store = Store::new();
        assert_eq!(store.total_requests(), 0);

        store.apply(&request_start_line(1_000, 1));
        store.apply(&request_start_line(1_000, 1));
        store.apply(&request_done_line(2_000, 1));
        store.apply(&request_done_line(2_000, 1));
        store.apply(&request_done_line(3_000, 2));

        assert_eq!(store.total_requests(), 2);
        assert_eq!(store.total_done(), 2);
        assert_eq!(store.active_requests(), 0);
    }

    #[test]
    fn late_done_for_evicted_request_does_not_inflate_total_requests() {
        let cap = 10;
        let mut store = Store::with_max_requests(cap);
        store.apply(&request_start_line(1_000, 1));
        store.apply(&request_done_line(2_000, 1));
        for id in 2..=(cap + 1) as u64 {
            store.apply(&request_start_line(id * 1_000, id));
            store.apply(&request_done_line(id * 1_000 + 500, id));
        }
        assert!(store.request(1).is_none());
        assert_eq!(store.total_requests(), cap + 1);

        store.apply(&request_done_line(3_000, 1));
        assert_eq!(store.total_requests(), cap + 1);
    }

    #[test]
    fn seen_ids_is_bounded_while_total_requests_stays_unique() {
        let cap = 100;
        let mut store = Store::with_max_requests(cap);
        // Distinct ids far beyond the retain window (cap * factor).
        for id in 1..=1_000 {
            store.apply(&request_start_line(id * 1_000, id));
            store.apply(&request_done_line(id * 1_000 + 500, id));
        }
        // Every distinct id is still counted exactly once.
        assert_eq!(store.total_requests(), 1_000);
        // The id set is bounded, so memory stays flat on a long-running server.
        let retain = cap * SEEN_IDS_RETAIN_FACTOR;
        assert!(
            store.seen_ids.len() <= retain,
            "seen_ids grew to {}, expected <= {}",
            store.seen_ids.len(),
            retain
        );
        assert_eq!(store.seen_order.len(), store.seen_ids.len());
    }

    #[test]
    fn recent_ids_survive_pruning_but_oldest_are_forgotten() {
        let cap = 2;
        let mut store = Store::with_max_requests(cap);
        let retain = cap * SEEN_IDS_RETAIN_FACTOR;
        for id in 1..=(retain as u64 + 5) {
            store.apply(&request_done_line(id * 1_000, id));
        }
        assert_eq!(store.total_requests(), retain + 5);
        assert_eq!(store.seen_ids.len(), retain);
        // The newest ids are retained.
        assert!(store.seen_ids.contains(&(retain as u64 + 5)));
        // The oldest ids were forgotten.
        assert!(!store.seen_ids.contains(&1));
    }

    #[test]
    fn seen_ids_is_bounded_with_zero_row_cap() {
        let mut store = Store::with_max_requests(0);
        for id in 1..=100 {
            store.apply(&request_done_line(id * 1_000, id));
        }
        assert_eq!(store.total_requests(), 100);
        assert_eq!(store.seen_ids.len(), 1);
        assert!(store.seen_ids.contains(&100));
    }
}
