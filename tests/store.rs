// NInferMonitor
//
// This is free software; see the LICENSE.md file in the source distribution for precise wording.
//
// Copyright (C) 2026 Aleksey Sanin aleksey@aleksey.com. All Rights Reserved.

use std::path::{Path, PathBuf};

use ninfer_monitor::config::DEFAULT_MAX_REQUESTS;
use ninfer_monitor::metrics::MAX_SERIES_POINTS;
use ninfer_monitor::parser::{ParsedEvent, parse_line};
use ninfer_monitor::store::Store;

fn sample_log_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/server.requests.jsonl")
}

fn feed_sample_log() -> Store {
    Store::load_jsonl(sample_log_path()).unwrap()
}

fn assert_close(actual: Option<f64>, expected: f64) {
    let actual = actual.expect("value present");
    assert!(
        (actual - expected).abs() < 1e-9,
        "expected {expected}, got {actual}"
    );
}

#[test]
fn sample_log_request_counts() {
    let store = feed_sample_log();
    assert_eq!(store.total_done(), 32);
    assert_eq!(store.total_error(), 2);
    assert_eq!(store.total_closed(), 34);
    assert_eq!(store.total_requests(), 34);
    assert_eq!(store.active_requests(), 0);
    assert_eq!(store.skipped_lines(), 0);
}

#[test]
fn sample_log_windowed_metrics_match_hand_computed() {
    let store = feed_sample_log();
    let w = store.window();
    // Last 60 s of the sample log (anchor 1789304713554) contains 11
    // completed requests and 0 errors.
    assert_eq!(w.request_count(), 11);
    assert_eq!(w.error_count(), 0);
    assert_eq!(w.cutoff(), Some(1789304653554));
    assert_eq!(w.range(), Some((1789304653554, 1789304713554)));
    assert_close(w.avg_ttft(), 0.4456230272727273);
    assert_close(w.avg_decode_latency_per_token(), 0.007278349731205014);
    assert_close(w.prefix_cache_hit_rate(), 0.9969298439400317);
    assert_close(w.speculative_acceptance(), 0.8274085522926327);
}

#[test]
fn sample_log_latest_throughput() {
    let store = feed_sample_log();
    let tp = store.latest_throughput().expect("throughput sample");
    assert_eq!(tp.timestamp_ms, 1789304713554);
    assert_close(tp.decode_tps, 123.89240590071061);
    assert_eq!(tp.prefill_tps, Some(0.0));
    assert_eq!(tp.scheduler_running, Some(0));
    assert_eq!(tp.device_main_kv_pages, Some(3261));
    assert_eq!(tp.device_backend_kv_pages, Some(3263));
    assert_eq!(tp.host_kv_bytes, Some(6368083968));
}

#[test]
fn sample_log_throughput_history_is_complete() {
    let store = feed_sample_log();
    // 51 throughput events, well under the 10 000 point cap.
    assert_eq!(store.throughput().len(), 51);
    let first = store.throughput().iter().next().expect("first sample");
    assert_close(first.decode_tps, 53.71620426468579);
    assert_close(first.prefill_tps, 2461.725450667429);
}

#[test]
fn sample_log_server_info() {
    let store = feed_sample_log();
    let server = store.server().expect("server info");
    assert_eq!(
        server.environment.gpu_name.as_deref(),
        Some("NVIDIA GeForce RTX 5090")
    );
    assert_eq!(server.engine.kv_capacity, Some(240000));
    assert_eq!(server.engine.max_concurrency, Some(2));
    assert_eq!(store.server_start_count(), 1);
    assert!(store.restarts().is_empty());
}

#[test]
fn sample_log_request_list_newest_first() {
    let store = feed_sample_log();
    // 34 closed requests, all under the 1 000 row cap.
    assert_eq!(store.request_count(), 34);
    assert_eq!(store.dropped_requests(), 0);
    let ids: Vec<u64> = store.request_list().map(|s| s.id).collect();
    assert_eq!(ids.first().copied(), Some(653));
    assert_eq!(ids.last().copied(), Some(1));
}

#[test]
fn sample_log_request_one_detail() {
    let store = feed_sample_log();
    let state = store.request(1).expect("request 1");
    let done = state.done.as_ref().expect("request 1 done");
    assert_eq!(done.result.prompt_tokens, Some(645));
    assert_eq!(done.result.completion_tokens, Some(155));
    assert_eq!(done.timings_seconds.ttft, Some(0.4219088));
    assert_eq!(done.speculative.accepted_tokens, Some(109));
}

#[test]
fn synthetic_throughput_events_keep_ring_buffer_bounded() {
    let mut store = Store::new();
    let n = MAX_SERIES_POINTS + 500;
    for ts in 0..n {
        let raw = format!(
            r#"{{"artifact_type":"ninfer_serve_request_log","schema_version":20,"server_instance_id":"s","timestamp_unix_ms":{ts},"event":"throughput","throughput_tokens_per_second":{{"decode":1.0,"prefill":2.0}}}}"#
        );
        store.apply(&parse_line(raw.as_bytes()).unwrap());
    }
    assert_eq!(store.throughput().len(), MAX_SERIES_POINTS);
    assert_eq!(
        store.latest_throughput().expect("latest").timestamp_ms,
        (n - 1) as u64
    );
}

#[test]
fn synthetic_requests_keep_request_list_bounded() {
    let cap = DEFAULT_MAX_REQUESTS as usize;
    let mut store = Store::with_max_requests(cap);
    let n = cap + 500;
    for id in 1..=n as u64 {
        let start = format!(
            r#"{{"artifact_type":"ninfer_serve_request_log","schema_version":20,"server_instance_id":"s","timestamp_unix_ms":{id},"event":"request_start","request":{{"request_id":{id}}}}}"#
        );
        store.apply(&parse_line(start.as_bytes()).unwrap());
        let done = format!(
            r#"{{"artifact_type":"ninfer_serve_request_log","schema_version":20,"server_instance_id":"s","timestamp_unix_ms":{id},"event":"request_done","request":{{"request_id":{id}}},"result":{{"prompt_tokens":10,"completion_tokens":5}},"timings_seconds":{{"ttft":0.1,"decode":0.5,"total":0.6}}}}"#
        );
        store.apply(&parse_line(done.as_bytes()).unwrap());
    }
    assert_eq!(store.request_count(), cap);
    assert_eq!(store.dropped_requests(), 500);
    assert_eq!(store.total_done(), n);
    let first = store.request_list().next().expect("newest row");
    assert_eq!(first.id, n as u64);
}

#[test]
fn mixed_event_stream_updates_all_state() {
    let mut store = Store::new();
    let events: Vec<ParsedEvent> = vec![
        parse_line(
            br#"{"artifact_type":"ninfer_serve_request_log","schema_version":20,"server_instance_id":"s","timestamp_unix_ms":1000,"event":"server_start","engine":{"kv_capacity":100}}"#,
        )
        .unwrap(),
        parse_line(
            br#"{"artifact_type":"ninfer_serve_request_log","schema_version":20,"server_instance_id":"s","timestamp_unix_ms":2000,"event":"request_start","request":{"request_id":1}}"#,
        )
        .unwrap(),
        parse_line(
            br#"{"artifact_type":"ninfer_serve_request_log","schema_version":20,"server_instance_id":"s","timestamp_unix_ms":3000,"event":"throughput","throughput_tokens_per_second":{"decode":9.5,"prefill":0.0}}"#,
        )
        .unwrap(),
        parse_line(
            br#"{"artifact_type":"ninfer_serve_request_log","schema_version":20,"server_instance_id":"s","timestamp_unix_ms":4000,"event":"request_done","request":{"request_id":1},"result":{"prompt_tokens":10,"completion_tokens":2},"timings_seconds":{"ttft":0.25,"decode":0.5,"total":0.75}}"#,
        )
        .unwrap(),
    ];
    for event in &events {
        store.apply(event);
    }
    let server = store.server().expect("server info");
    assert_eq!(server.engine.kv_capacity, Some(100));
    assert_eq!(store.total_done(), 1);
    assert_eq!(store.active_requests(), 0);
    assert_eq!(store.latest_throughput().expect("tp").decode_tps, Some(9.5));
    assert_close(store.window().avg_ttft(), 0.25);
    assert_eq!(store.max_timestamp_ms(), Some(4000));
}
