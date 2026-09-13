// NInferMonitor
//
// This is free software; see the LICENSE.md file in the source distribution for precise wording.
//
// Copyright (C) 2026 Aleksey Sanin aleksey@aleksey.com. All Rights Reserved.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ninfer_monitor::pipeline::{Pipeline, StoreUpdate};
use ninfer_monitor::tail::TailStatus;

const POLL: Duration = Duration::from_millis(50);
const WAIT: Duration = Duration::from_secs(5);
const MAX_REQUESTS: usize = ninfer_monitor::config::DEFAULT_MAX_REQUESTS as usize;

fn sample_log_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/server.requests.jsonl")
}

fn append_line(path: &Path, line: &str) {
    let mut file = OpenOptions::new().append(true).open(path).unwrap();
    file.write_all(format!("{line}\n").as_bytes()).unwrap();
}

fn throughput_line(ts: u64) -> String {
    format!(
        r#"{{"artifact_type":"ninfer_serve_request_log","schema_version":20,"server_instance_id":"s","timestamp_unix_ms":{ts},"event":"throughput","throughput_tokens_per_second":{{"decode":9.5,"prefill":0.0}}}}"#
    )
}

/// Wait until the latest snapshot satisfies `pred`. Snapshots are full store
/// clones and the store only grows within one server session, so the newest
/// snapshot is the most complete one seen so far.
fn wait_for(
    pipeline: &Pipeline,
    mut pred: impl FnMut(&StoreUpdate) -> bool,
) -> Option<StoreUpdate> {
    let deadline = Instant::now() + WAIT;
    while Instant::now() < deadline {
        if let Some(update) = pipeline.take_latest()
            && pred(&update)
        {
            return Some(update);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    None
}

#[test]
fn backfill_sample_log_updates_store() {
    let pipeline = Pipeline::start(sample_log_path(), POLL, MAX_REQUESTS);
    let update = wait_for(&pipeline, |u| u.store.total_done() == 32)
        .expect("store never reached 32 done requests");
    assert_eq!(update.status, TailStatus::Live);
    assert_eq!(update.store.total_error(), 2);
    assert_eq!(update.store.total_requests(), 34);
    assert_eq!(update.store.request_count(), 34);
    assert_eq!(update.store.throughput().len(), 51);
    assert_eq!(update.store.skipped_lines(), 0);
    assert!(update.last_event_time.is_some());
    let tp = update.store.latest_throughput().expect("latest sample");
    assert_eq!(tp.timestamp_ms, 1789304713554);
    pipeline.stop();
}

#[test]
fn live_append_updates_store_within_five_seconds() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("log.jsonl");
    File::create(&path).unwrap();

    let pipeline = Pipeline::start(&path, POLL, MAX_REQUESTS);
    wait_for(&pipeline, |u| u.status == TailStatus::Live).expect("never went Live");

    let start = Instant::now();
    append_line(&path, &throughput_line(1000));
    let update = wait_for(&pipeline, |u| u.store.latest_throughput().is_some())
        .expect("throughput line never arrived");
    let elapsed = start.elapsed();
    assert!(elapsed < WAIT, "store update took {elapsed:?}");
    assert_eq!(
        update
            .store
            .latest_throughput()
            .expect("sample")
            .timestamp_ms,
        1000
    );
    pipeline.stop();
}

#[test]
fn request_lifecycle_flows_through_pipeline() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("log.jsonl");
    File::create(&path).unwrap();

    let pipeline = Pipeline::start(&path, POLL, MAX_REQUESTS);
    wait_for(&pipeline, |u| u.status == TailStatus::Live).expect("never went Live");

    let start = r#"{"artifact_type":"ninfer_serve_request_log","schema_version":20,"server_instance_id":"s","timestamp_unix_ms":1000,"event":"request_start","request":{"request_id":7,"model":"m"}}"#;
    append_line(&path, start);
    let update = wait_for(&pipeline, |u| u.store.active_requests() == 1)
        .expect("in-flight request never appeared");
    assert_eq!(
        update
            .store
            .request(7)
            .map(|s| s.status)
            .map(|s| format!("{s:?}")),
        Some("InFlight".to_string())
    );

    let done = r#"{"artifact_type":"ninfer_serve_request_log","schema_version":20,"server_instance_id":"s","timestamp_unix_ms":2000,"event":"request_done","request":{"request_id":7},"result":{"prompt_tokens":100,"completion_tokens":50},"timings_seconds":{"ttft":0.5,"decode":1.0,"total":1.5}}"#;
    append_line(&path, done);
    let update =
        wait_for(&pipeline, |u| u.store.total_done() == 1).expect("done request never arrived");
    assert_eq!(update.store.active_requests(), 0);
    assert_eq!(
        update
            .store
            .request(7)
            .and_then(|s| s.done.as_ref())
            .and_then(|d| d.result.prompt_tokens),
        Some(100)
    );
    pipeline.stop();
}

#[test]
fn malformed_lines_are_counted_and_valid_lines_applied() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("log.jsonl");
    File::create(&path).unwrap();

    let pipeline = Pipeline::start(&path, POLL, MAX_REQUESTS);
    wait_for(&pipeline, |u| u.status == TailStatus::Live).expect("never went Live");

    append_line(&path, "{\"broken");
    append_line(&path, "not json at all");
    append_line(&path, &throughput_line(1000));
    let update = wait_for(&pipeline, |u| {
        u.store.skipped_lines() == 2 && u.store.throughput().len() == 1
    })
    .expect("skipped counter and valid line never both arrived");
    assert_eq!(update.store.throughput().len(), 1);
    pipeline.stop();
}

#[test]
fn delete_file_reports_disconnected_then_live_on_recreate() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("log.jsonl");
    File::create(&path).unwrap();

    let pipeline = Pipeline::start(&path, POLL, MAX_REQUESTS);
    wait_for(&pipeline, |u| u.status == TailStatus::Live).expect("never went Live");

    fs::remove_file(&path).unwrap();
    wait_for(&pipeline, |u| u.status == TailStatus::Disconnected)
        .expect("never reported Disconnected");

    File::create(&path).unwrap();
    wait_for(&pipeline, |u| u.status == TailStatus::Live).expect("never back to Live");
    pipeline.stop();
}

#[test]
fn missing_file_reports_disconnected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("does-not-exist.jsonl");

    let pipeline = Pipeline::start(&path, POLL, MAX_REQUESTS);
    let update = wait_for(&pipeline, |u| u.status == TailStatus::Disconnected)
        .expect("never reported Disconnected");
    assert!(update.store.latest_throughput().is_none());
    pipeline.stop();
}

#[test]
fn snapshots_are_coalesced_to_at_most_4hz() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("log.jsonl");
    File::create(&path).unwrap();

    let pipeline = Pipeline::start(&path, POLL, MAX_REQUESTS);
    wait_for(&pipeline, |u| u.status == TailStatus::Live).expect("never went Live");
    while pipeline.take_latest().is_some() {}

    for i in 0..200 {
        append_line(&path, &throughput_line(i as u64));
    }

    let start = Instant::now();
    let mut count = 0;
    let mut last = None;
    while start.elapsed() < Duration::from_millis(1200) {
        if let Some(update) = pipeline.take_latest() {
            count += 1;
            last = Some(update);
        } else {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    let last = last.expect("no snapshots received");
    assert_eq!(last.store.throughput().len(), 200);
    assert!(
        count <= 8,
        "expected at most 8 snapshots over 1.2 s (got {count}); \
         without coalescing this would be ~200"
    );
    pipeline.stop();
}

#[test]
fn stop_joins_threads_promptly_even_with_long_poll_interval() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("log.jsonl");
    File::create(&path).unwrap();

    let pipeline = Pipeline::start(&path, Duration::from_secs(5), MAX_REQUESTS);
    let start = Instant::now();
    pipeline.stop();
    let elapsed = start.elapsed();
    assert!(elapsed < Duration::from_secs(1), "stop took {elapsed:?}");
}

#[test]
fn take_latest_returns_newest_pending_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("log.jsonl");
    File::create(&path).unwrap();

    let pipeline = Pipeline::start(&path, POLL, MAX_REQUESTS);
    wait_for(&pipeline, |u| u.status == TailStatus::Live).expect("never went Live");

    append_line(&path, &throughput_line(1000));
    append_line(&path, &throughput_line(2000));
    let deadline = Instant::now() + WAIT;
    let mut last = None;
    while Instant::now() < deadline {
        if let Some(update) = pipeline.take_latest() {
            let complete = update.store.throughput().len() == 2;
            last = Some(update);
            if complete {
                break;
            }
        } else {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    let last = last.expect("no snapshot with both lines");
    assert_eq!(last.store.throughput().len(), 2);
    assert_eq!(
        last.store.latest_throughput().expect("sample").timestamp_ms,
        2000
    );
    pipeline.stop();
}

#[test]
fn clear_resets_store_and_bumps_clear_count() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("log.jsonl");
    File::create(&path).unwrap();

    let pipeline = Pipeline::start(&path, POLL, MAX_REQUESTS);
    wait_for(&pipeline, |u| u.status == TailStatus::Live).expect("never went Live");

    append_line(&path, &throughput_line(1000));
    let update = wait_for(&pipeline, |u| u.store.latest_throughput().is_some())
        .expect("throughput line never arrived");
    assert_eq!(update.clear_count, 0);

    pipeline.clear();
    let update = wait_for(&pipeline, |u| u.clear_count == 1).expect("clear snapshot never arrived");
    assert!(update.store.throughput().is_empty());
    assert!(update.last_event_time.is_none());
    assert_eq!(update.status, TailStatus::Live);

    append_line(&path, &throughput_line(2000));
    let update = wait_for(&pipeline, |u| u.store.latest_throughput().is_some())
        .expect("post-clear line never arrived");
    assert_eq!(update.clear_count, 1);
    assert_eq!(
        update
            .store
            .latest_throughput()
            .expect("sample")
            .timestamp_ms,
        2000
    );
    pipeline.stop();
}

#[test]
fn second_server_start_resets_store_through_pipeline() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("log.jsonl");
    File::create(&path).unwrap();

    let pipeline = Pipeline::start(&path, POLL, MAX_REQUESTS);
    wait_for(&pipeline, |u| u.status == TailStatus::Live).expect("never went Live");

    let start1 = r#"{"artifact_type":"ninfer_serve_request_log","schema_version":20,"server_instance_id":"s1","timestamp_unix_ms":1000,"event":"server_start"}"#;
    append_line(&path, start1);
    let req_start = r#"{"artifact_type":"ninfer_serve_request_log","schema_version":20,"server_instance_id":"s1","timestamp_unix_ms":2000,"event":"request_start","request":{"request_id":1,"model":"m"}}"#;
    append_line(&path, req_start);
    let req_done = r#"{"artifact_type":"ninfer_serve_request_log","schema_version":20,"server_instance_id":"s1","timestamp_unix_ms":3000,"event":"request_done","request":{"request_id":1},"result":{"prompt_tokens":100,"completion_tokens":50},"timings_seconds":{"ttft":0.5,"decode":1.0,"total":1.5}}"#;
    append_line(&path, req_done);
    let update =
        wait_for(&pipeline, |u| u.store.total_done() == 1).expect("done request never arrived");
    assert_eq!(update.store.server_start_count(), 1);
    assert_eq!(
        update
            .store
            .server()
            .and_then(|s| s.server_instance_id.as_deref()),
        Some("s1")
    );

    let start2 = r#"{"artifact_type":"ninfer_serve_request_log","schema_version":20,"server_instance_id":"s2","timestamp_unix_ms":5000,"event":"server_start"}"#;
    append_line(&path, start2);
    let update =
        wait_for(&pipeline, |u| u.store.server_start_count() == 2).expect("restart never arrived");
    assert_eq!(update.store.restarts(), &[5000]);
    assert_eq!(update.store.total_done(), 0);
    assert_eq!(update.store.total_requests(), 0);
    assert_eq!(update.store.request_count(), 0);
    assert_eq!(
        update
            .store
            .server()
            .and_then(|s| s.server_instance_id.as_deref()),
        Some("s2")
    );
    pipeline.stop();
}
