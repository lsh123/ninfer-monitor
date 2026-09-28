// NInferMonitor
//
// This is free software; see the LICENSE.md file in the source distribution for precise wording.
//
// Copyright (C) 2026 Aleksey Sanin aleksey@aleksey.com. All Rights Reserved.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

use crate::parser::parse_line;
use crate::store::Store;
use crate::tail::{Tail, TailMessage, TailStatus, spawn as spawn_tail};

/// Maximum interval between store snapshots pushed to the UI (4 Hz, PRD §11).
pub const PUSH_INTERVAL: Duration = Duration::from_millis(250);

/// A snapshot of the store plus tail status, pushed to the UI over a channel.
///
/// The store is a full clone: the UI thread never shares mutable state with
/// the parse thread (PRD §11).
#[derive(Debug, Clone)]
pub struct StoreUpdate {
    pub store: Store,
    pub status: TailStatus,
    pub last_event_time: Option<SystemTime>,
}

/// Folds tail messages into a store.
///
/// This is the thread-free core of the parse loop, kept separate so the
/// message handling is unit-testable without spawning threads.
#[derive(Debug)]
pub struct Processor {
    store: Store,
    status: TailStatus,
    last_event_time: Option<SystemTime>,
}

impl Processor {
    /// A processor with the default request-row cap (FR-5.5).
    pub fn new() -> Self {
        Self::with_max_requests(crate::config::DEFAULT_MAX_REQUESTS as usize)
    }

    /// A processor whose store keeps at most `max_requests` request rows
    /// (FR-5.5).
    pub fn with_max_requests(max_requests: usize) -> Self {
        Self {
            store: Store::with_max_requests(max_requests),
            status: TailStatus::Disconnected,
            last_event_time: None,
        }
    }

    pub fn handle(&mut self, message: &TailMessage) {
        match message {
            TailMessage::Line(line) => match parse_line(line) {
                Ok(event) => self.store.apply(&event),
                Err(_) => self.store.note_skipped_line(),
            },
            TailMessage::Status {
                status,
                last_event_time,
            } => {
                self.status = *status;
                self.last_event_time = *last_event_time;
            }
        }
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn status(&self) -> TailStatus {
        self.status
    }

    pub fn last_event_time(&self) -> Option<SystemTime> {
        self.last_event_time
    }

    /// Clone the store to hand to the UI thread. The clone is the cost of the
    /// owned-snapshot design (the `Store` crosses the thread boundary as an
    /// owned value, so it cannot be shared), but it is bounded: a snapshot is
    /// only pushed on activity at ≤ 4 Hz, and the UI drains the channel
    /// (`take_latest`), keeping at most one pending clone.
    pub fn snapshot(&self) -> StoreUpdate {
        StoreUpdate {
            store: self.store.clone(),
            status: self.status,
            last_event_time: self.last_event_time,
        }
    }
}

impl Default for Processor {
    fn default() -> Self {
        Self::new()
    }
}

/// Parse thread body: consumes tail messages, updates the store, and pushes
/// a snapshot at most every `PUSH_INTERVAL` (≤ 4 Hz).
///
/// The push schedule is fixed (not reset by incoming messages) so a fast log
/// cannot flood the UI channel: at most one snapshot per 250 ms window, and
/// a line written at time T is included in the snapshot pushed by T + 250 ms.
fn parse_loop(
    rx: mpsc::Receiver<TailMessage>,
    tx: mpsc::Sender<StoreUpdate>,
    stop: Arc<AtomicBool>,
    max_requests: usize,
) {
    let mut processor = Processor::with_max_requests(max_requests);
    let mut pending = false;
    let mut next_push = Instant::now() + PUSH_INTERVAL;
    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let wait = next_push.saturating_duration_since(Instant::now());
        match rx.recv_timeout(wait) {
            Ok(message) => {
                processor.handle(&message);
                pending = true;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        while let Ok(message) = rx.try_recv() {
            processor.handle(&message);
            pending = true;
        }
        let now = Instant::now();
        if now >= next_push {
            if pending {
                if tx.send(processor.snapshot()).is_err() {
                    break;
                }
                pending = false;
            }
            next_push = now + PUSH_INTERVAL;
        }
    }
}

/// The tail → parse → store pipeline (PRD §11).
///
/// The tail thread reads lines from the log file; the parse thread
/// deserializes them and updates the store; `StoreUpdate` snapshots are
/// pushed over an mpsc channel for the UI to apply at ≤ 4 Hz. All mutable
/// state is owned by a single thread; only owned values cross thread
/// boundaries.
pub struct Pipeline {
    rx: Mutex<mpsc::Receiver<StoreUpdate>>,
    stop: Arc<AtomicBool>,
    tail_handle: Option<JoinHandle<()>>,
    parse_handle: Option<JoinHandle<()>>,
    poll_interval: Duration,
    max_requests: usize,
}

impl Pipeline {
    /// Start the tail and parse threads for `path`, polling every
    /// `poll_interval`, keeping at most `max_requests` request rows (FR-5.5).
    pub fn start(path: impl Into<PathBuf>, poll_interval: Duration, max_requests: usize) -> Self {
        let (tail_tx, tail_rx) = mpsc::channel();
        let (update_tx, update_rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let tail_handle = spawn_tail(Tail::new(path, poll_interval), tail_tx, stop.clone());
        let parse_stop = stop.clone();
        let parse_handle = thread::Builder::new()
            .name("parse".to_string())
            .spawn(move || parse_loop(tail_rx, update_tx, parse_stop, max_requests))
            .expect("failed to spawn parse thread");
        Self {
            rx: Mutex::new(update_rx),
            stop,
            tail_handle: Some(tail_handle),
            parse_handle: Some(parse_handle),
            poll_interval,
            max_requests,
        }
    }

    /// The poll interval the tail thread uses (FR-1.2).
    pub fn poll_interval(&self) -> Duration {
        self.poll_interval
    }

    /// The request-row cap the store keeps (FR-5.5).
    pub fn max_requests(&self) -> usize {
        self.max_requests
    }

    /// Take the newest pending snapshot without blocking, dropping older
    /// ones. The UI calls this on its ≤ 4 Hz timer.
    pub fn take_latest(&self) -> Option<StoreUpdate> {
        latest_update(&self.rx)
    }

    /// Signal the threads to stop and wait for them to exit.
    pub fn stop(mut self) {
        self.stop_inner();
    }

    fn stop_inner(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.parse_handle.take() {
            let _ = handle.join();
        }
        if let Some(handle) = self.tail_handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for Pipeline {
    /// A dropped pipeline stops and joins its threads (bounded: the parse
    /// thread wakes within `PUSH_INTERVAL`, the tail thread within one sleep
    /// slice), so a replaced pipeline (startup Start, Settings Save) doesn't
    /// leave detached threads reading the old file.
    fn drop(&mut self) {
        self.stop_inner();
    }
}

/// Drain a receiver and return the newest update (if any).
pub fn latest_update(rx: &Mutex<mpsc::Receiver<StoreUpdate>>) -> Option<StoreUpdate> {
    let rx = rx.lock().unwrap();
    let mut latest = None;
    while let Ok(update) = rx.try_recv() {
        latest = Some(update);
    }
    latest
}

/// The last-event time for the status bar (FR-1.7), local time with
/// millisecond precision (NFR-5).
pub fn format_last_event(time: SystemTime) -> String {
    let local: chrono::DateTime<chrono::Local> = time.into();
    local.format("%H:%M:%S%.3f").to_string()
}

/// The install-folder name for the status bar (PRD v0.3 §3.7, M2): names
/// longer than 32 characters keep 15 characters on each side with an
/// ellipsis in the middle.
pub fn shorten_file_name(name: &str) -> String {
    let chars: Vec<char> = name.chars().collect();
    if chars.len() <= 32 {
        return name.to_string();
    }
    let left: String = chars[..15].iter().collect();
    let right: String = chars[chars.len() - 15..].iter().collect();
    format!("{left}…{right}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(s: &str) -> TailMessage {
        TailMessage::Line(s.as_bytes().to_vec())
    }

    fn throughput_line(ts: u64) -> String {
        format!(
            r#"{{"artifact_type":"ninfer_serve_request_log","schema_version":20,"server_instance_id":"s","timestamp_unix_ms":{ts},"event":"throughput","throughput_tokens_per_second":{{"decode":1.5,"prefill":2.5}}}}"#
        )
    }

    #[test]
    fn processor_starts_disconnected_with_empty_store() {
        let p = Processor::new();
        assert_eq!(p.status(), TailStatus::Disconnected);
        assert!(p.last_event_time().is_none());
        assert!(p.store().latest_throughput().is_none());
        assert_eq!(p.store().skipped_lines(), 0);
    }

    #[test]
    fn processor_applies_valid_lines() {
        let mut p = Processor::new();
        p.handle(&line(&throughput_line(1000)));
        p.handle(&line(&throughput_line(2000)));
        assert_eq!(p.store().throughput().len(), 2);
        assert_eq!(
            p.store().latest_throughput().expect("sample").timestamp_ms,
            2000
        );
    }

    #[test]
    fn processor_counts_malformed_lines() {
        let mut p = Processor::new();
        p.handle(&line("{\"broken"));
        p.handle(&line("not json at all"));
        p.handle(&line(&throughput_line(1000)));
        assert_eq!(p.store().skipped_lines(), 2);
        assert_eq!(p.store().throughput().len(), 1);
    }

    #[test]
    fn processor_tracks_status_messages() {
        let mut p = Processor::new();
        let t = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
        p.handle(&TailMessage::Status {
            status: TailStatus::Live,
            last_event_time: Some(t),
        });
        assert_eq!(p.status(), TailStatus::Live);
        assert_eq!(p.last_event_time(), Some(t));
    }

    #[test]
    fn snapshot_is_an_independent_store_clone() {
        let mut p = Processor::new();
        p.handle(&line(&throughput_line(1000)));
        let snap = p.snapshot();
        assert_eq!(snap.store.throughput().len(), 1);
        p.handle(&line(&throughput_line(2000)));
        assert_eq!(snap.store.throughput().len(), 1);
        assert_eq!(p.store().throughput().len(), 2);
    }

    #[test]
    fn latest_update_returns_newest_pending() {
        let (tx, rx) = mpsc::channel();
        let rx = Mutex::new(rx);
        let mut p = Processor::new();
        p.handle(&line(&throughput_line(1000)));
        tx.send(p.snapshot()).unwrap();
        p.handle(&line(&throughput_line(2000)));
        tx.send(p.snapshot()).unwrap();
        let latest = latest_update(&rx).expect("an update");
        assert_eq!(latest.store.throughput().len(), 2);
        assert!(latest_update(&rx).is_none());
    }

    #[test]
    fn shorten_file_name_keeps_short_names() {
        assert_eq!(
            shorten_file_name("server.requests.jsonl"),
            "server.requests.jsonl"
        );
        let name = "a".repeat(32);
        assert_eq!(shorten_file_name(&name), name);
    }

    #[test]
    fn shorten_file_name_ellipsizes_long_names_in_the_middle() {
        let name = "d:/NInfer/logs/server.requests.jsonl";
        assert_eq!(shorten_file_name(name), "d:/NInfer/logs/….requests.jsonl");
    }

    #[test]
    fn shorten_file_name_uses_chars_not_bytes() {
        let name = format!("…{}", "a".repeat(31));
        assert_eq!(shorten_file_name(&name), name);
        let name = format!("…{}", "a".repeat(32));
        let expected = format!("…{}…{}", "a".repeat(14), "a".repeat(15));
        assert_eq!(shorten_file_name(&name), expected);
    }
}
