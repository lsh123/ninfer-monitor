// NInferMonitor
//
// This is free software; see the LICENSE.md file in the source distribution for precise wording.
//
// Copyright (C) 2026 Aleksey Sanin aleksey@aleksey.com. All Rights Reserved.

use std::path::{Path, PathBuf};

use ninfer_monitor::chart::{self, ChartKind};
use ninfer_monitor::parser::parse_line;
use ninfer_monitor::store::Store;
use serde_json::Value;

const LOG: &str = "tests/fixtures/server.requests.jsonl";

fn log_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(LOG)
}

fn sample_store() -> Store {
    Store::load_jsonl(log_path()).expect("read sample log")
}

fn raw_events() -> Vec<Value> {
    let data = std::fs::read_to_string(log_path()).expect("read log");
    data.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("parse json line"))
        .collect()
}

fn max_ts(events: &[Value]) -> u64 {
    events
        .iter()
        .filter_map(|v| v.get("timestamp_unix_ms").and_then(Value::as_u64))
        .max()
        .expect("at least one timestamp")
}

fn throughput_events(events: &[Value]) -> Vec<&Value> {
    events
        .iter()
        .filter(|v| v.get("event").and_then(Value::as_str) == Some("throughput"))
        .collect()
}

fn in_window(ts: u64, t_start: u64, t_end: u64) -> bool {
    ts >= t_start && ts <= t_end
}

fn ts_of(v: &Value) -> Option<u64> {
    v.get("timestamp_unix_ms").and_then(Value::as_u64)
}

#[test]
fn throughput_chart_matches_raw_json() {
    let store = sample_store();
    let events = raw_events();
    let t_end = max_ts(&events);
    let t_start = t_end.saturating_sub(300_000);
    let expected: Vec<(f64, f64)> = throughput_events(&events)
        .iter()
        .filter_map(|v| {
            let ts = ts_of(v)?;
            if !in_window(ts, t_start, t_end) {
                return None;
            }
            let dec = v
                .get("throughput_tokens_per_second")?
                .get("decode")?
                .as_f64()?;
            Some((ts as f64, dec))
        })
        .collect();
    assert!(
        expected.len() >= 3,
        "need >=3 reference points, got {}",
        expected.len()
    );
    let data = chart::prepare(&store, ChartKind::Throughput, 300_000, t_end);
    let got = &data.series[1].points;
    assert!(got.len() >= 3, "chart has <3 decode points");
    for i in 0..3 {
        let (exp_ts, exp_v) = expected[expected.len() - 1 - i];
        let (act_ts, act_v) = got[got.len() - 1 - i];
        assert!(
            (exp_ts - act_ts).abs() < 1e-6,
            "last-{i} ts mismatch: {exp_ts} vs {act_ts}"
        );
        assert!(
            (exp_v - act_v).abs() < 1e-9,
            "last-{i} decode mismatch: {exp_v} vs {act_v}"
        );
    }
}

#[test]
fn ttft_chart_matches_raw_json() {
    let store = sample_store();
    let events = raw_events();
    let t_end = max_ts(&events);
    let t_start = t_end.saturating_sub(300_000);
    let expected: Vec<(f64, f64)> = events
        .iter()
        .filter(|v| v.get("event").and_then(Value::as_str) == Some("request_done"))
        .filter_map(|v| {
            let ts = ts_of(v)?;
            if !in_window(ts, t_start, t_end) {
                return None;
            }
            let ttft = v.get("timings_seconds")?.get("ttft")?.as_f64()?;
            Some((ts as f64, ttft * 1000.0))
        })
        .collect();
    assert!(
        expected.len() >= 3,
        "need >=3 reference points, got {}",
        expected.len()
    );
    let data = chart::prepare(&store, ChartKind::Latency, 300_000, t_end);
    let got = &data.series[0].points;
    assert!(got.len() >= 3, "chart has <3 ttft points");
    for i in 0..3 {
        let (exp_ts, exp_v) = expected[expected.len() - 1 - i];
        let (act_ts, act_v) = got[got.len() - 1 - i];
        assert!(
            (exp_ts - act_ts).abs() < 1e-6,
            "last-{i} ts mismatch: {exp_ts} vs {act_ts}"
        );
        assert!(
            (exp_v - act_v).abs() < 1e-6,
            "last-{i} ttft mismatch: {exp_v} vs {act_v}"
        );
    }
}

#[test]
fn per_token_latency_chart_matches_raw_json() {
    let store = sample_store();
    let events = raw_events();
    let t_end = max_ts(&events);
    let t_start = t_end.saturating_sub(300_000);
    let expected: Vec<(f64, f64)> = events
        .iter()
        .filter(|v| v.get("event").and_then(Value::as_str) == Some("request_done"))
        .filter_map(|v| {
            let ts = ts_of(v)?;
            if !in_window(ts, t_start, t_end) {
                return None;
            }
            let decode = v.get("timings_seconds")?.get("decode")?.as_f64()?;
            let completion = v.get("result")?.get("completion_tokens")?.as_u64()?;
            if completion == 0 {
                return None;
            }
            Some((ts as f64, decode / completion as f64 * 1000.0))
        })
        .collect();
    assert!(
        expected.len() >= 3,
        "need >=3 reference points, got {}",
        expected.len()
    );
    let data = chart::prepare(&store, ChartKind::Latency, 300_000, t_end);
    let got = &data.series[1].points;
    assert!(got.len() >= 3, "chart has <3 per-token points");
    for i in 0..3 {
        let (exp_ts, exp_v) = expected[expected.len() - 1 - i];
        let (act_ts, act_v) = got[got.len() - 1 - i];
        assert!(
            (exp_ts - act_ts).abs() < 1e-6,
            "last-{i} ts mismatch: {exp_ts} vs {act_ts}"
        );
        assert!(
            (exp_v - act_v).abs() < 1e-6,
            "last-{i} per-token mismatch: {exp_v} vs {act_v}"
        );
    }
}

#[test]
fn cache_chart_last_point_matches_raw_json() {
    let store = sample_store();
    let events = raw_events();
    let t_end = max_ts(&events);
    let t_start = t_end.saturating_sub(300_000);
    let done: Vec<&Value> = events
        .iter()
        .filter(|v| v.get("event").and_then(Value::as_str) == Some("request_done"))
        .filter(|v| ts_of(v).is_some_and(|ts| in_window(ts, t_start, t_end)))
        .collect();
    let last = done
        .iter()
        .max_by_key(|v| ts_of(v).unwrap())
        .expect("a request_done in the window");
    let ts_last = ts_of(last).unwrap();
    let lo = ts_last.saturating_sub(60_000);
    let (mut prompt, mut hit) = (0u64, 0u64);
    for v in &done {
        let ts = ts_of(v).unwrap();
        if ts >= lo && ts <= ts_last {
            let result = v.get("result").unwrap();
            prompt += result
                .get("prompt_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            hit += result
                .get("prefix_cache_hit_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0);
        }
    }
    let expected_rate = hit as f64 / prompt as f64 * 100.0;
    let data = chart::prepare(&store, ChartKind::Cache, 300_000, t_end);
    let got = &data.series[0].points;
    let (act_ts, act_v) = got.last().expect("a cache point");
    assert!(
        (ts_last as f64 - act_ts).abs() < 1e-6,
        "last cache ts mismatch"
    );
    assert!(
        (expected_rate - act_v).abs() < 1e-6,
        "last cache rate mismatch: {expected_rate} vs {act_v}"
    );
}

#[test]
fn cache_chart_has_windowed_rate_series() {
    let store = sample_store();
    let data = chart::prepare(&store, ChartKind::Cache, 300_000, 0);
    assert_eq!(data.series.len(), 2);
    assert_eq!(data.series[0].name, "cache hit");
    assert_eq!(data.series[1].name, "spec accept");
    assert!(data.series[0].points.len() >= 3, "need >=3 cache points");
    assert!(data.series[1].points.len() >= 3, "need >=3 spec points");
    for (t, v) in data.series[0]
        .points
        .iter()
        .chain(data.series[1].points.iter())
    {
        assert!(*t >= 0.0, "negative timestamp: {t}");
        assert!(*v >= 0.0 && *v <= 100.0, "rate out of [0,100]: {v}");
    }
}

#[test]
fn scheduler_chart_matches_raw_json() {
    let store = sample_store();
    let events = raw_events();
    let t_end = max_ts(&events);
    let t_start = t_end.saturating_sub(300_000);
    let expected: Vec<(f64, f64, f64, f64)> = throughput_events(&events)
        .iter()
        .filter_map(|v| {
            let ts = ts_of(v)?;
            if !in_window(ts, t_start, t_end) {
                return None;
            }
            let sched = v.get("scheduler")?;
            let running = sched.get("running").and_then(Value::as_u64).unwrap_or(0);
            let prefilling = sched.get("prefilling").and_then(Value::as_u64).unwrap_or(0);
            let waiting = sched.get("waiting").and_then(Value::as_u64).unwrap_or(0);
            Some((ts as f64, running as f64, prefilling as f64, waiting as f64))
        })
        .collect();
    assert!(
        expected.len() >= 3,
        "need >=3 reference points, got {}",
        expected.len()
    );
    let data = chart::prepare(&store, ChartKind::Scheduler, 300_000, t_end);
    let running = &data.series[0].points;
    let waiting = &data.series[1].points;
    assert!(
        running.len() >= 3 && waiting.len() >= 3,
        "chart has <3 scheduler points"
    );
    for i in 0..3 {
        let (exp_ts, exp_r, _exp_p, exp_w) = expected[expected.len() - 1 - i];
        let (act_ts, act_r) = running[running.len() - 1 - i];
        let (_, act_w) = waiting[waiting.len() - 1 - i];
        assert!((exp_ts - act_ts).abs() < 1e-6, "last-{i} ts mismatch");
        assert!((exp_r - act_r).abs() < 1e-6, "last-{i} running mismatch");
        assert!((exp_w - act_w).abs() < 1e-6, "last-{i} waiting mismatch");
    }
}

#[test]
fn window_filtering_matches_raw_json() {
    let store = sample_store();
    let events = raw_events();
    let t_end = max_ts(&events);
    for (window_ms, name) in [(300_000, "5m"), (3_600_000, "1h")] {
        let t_start = t_end.saturating_sub(window_ms);
        let expected_count = throughput_events(&events)
            .iter()
            .filter(|v| {
                ts_of(v).is_some_and(|ts| in_window(ts, t_start, t_end))
                    && v.get("throughput_tokens_per_second")
                        .and_then(|o| o.get("decode"))
                        .is_some()
            })
            .count();
        let data = chart::prepare(&store, ChartKind::Throughput, window_ms, t_end);
        assert_eq!(
            data.series[1].points.len(),
            expected_count,
            "{name} window decode point count mismatch"
        );
        let expected_prefill = throughput_events(&events)
            .iter()
            .filter(|v| {
                ts_of(v).is_some_and(|ts| in_window(ts, t_start, t_end))
                    && v.get("throughput_tokens_per_second")
                        .and_then(|o| o.get("prefill"))
                        .is_some()
            })
            .count();
        assert_eq!(
            data.series[0].points.len(),
            expected_prefill,
            "{name} window prefill point count mismatch"
        );
    }
}

#[test]
fn render_each_chart_under_50ms() {
    let store = sample_store();
    for kind in [
        ChartKind::Throughput,
        ChartKind::Latency,
        ChartKind::Cache,
        ChartKind::Scheduler,
    ] {
        let data = chart::prepare(
            &store,
            kind,
            3_600_000,
            store.max_timestamp_ms().unwrap_or(0),
        );
        let mut durations = Vec::new();
        for _ in 0..5 {
            let start = std::time::Instant::now();
            let _ = chart::render(&data, (640, 240));
            durations.push(start.elapsed());
        }
        durations.sort();
        let median = durations[durations.len() / 2];
        assert!(
            median < std::time::Duration::from_millis(50),
            "{kind:?} median render took {median:?}"
        );
    }
}

#[test]
fn empty_store_has_no_points() {
    let store = Store::new();
    for kind in [
        ChartKind::Throughput,
        ChartKind::Latency,
        ChartKind::Cache,
        ChartKind::Scheduler,
    ] {
        let data = chart::prepare(&store, kind, 300_000, 0);
        assert!(
            data.series.iter().all(|s| s.points.is_empty()),
            "{kind:?} should be empty"
        );
    }
}

#[test]
fn restart_marker_included_for_all_charts() {
    let mut store = Store::new();
    let raw1 = r#"{"artifact_type":"ninfer_serve_request_log","schema_version":20,"server_instance_id":"s1","timestamp_unix_ms":1000,"event":"server_start"}"#;
    store.apply(&parse_line(raw1.as_bytes()).unwrap());
    let tp1 = r#"{"artifact_type":"ninfer_serve_request_log","schema_version":20,"server_instance_id":"s1","timestamp_unix_ms":2000,"event":"throughput","throughput_tokens_per_second":{"decode":10.0,"prefill":20.0}}"#;
    store.apply(&parse_line(tp1.as_bytes()).unwrap());
    let raw2 = r#"{"artifact_type":"ninfer_serve_request_log","schema_version":20,"server_instance_id":"s2","timestamp_unix_ms":5000,"event":"server_start"}"#;
    store.apply(&parse_line(raw2.as_bytes()).unwrap());
    let tp2 = r#"{"artifact_type":"ninfer_serve_request_log","schema_version":20,"server_instance_id":"s2","timestamp_unix_ms":6000,"event":"throughput","throughput_tokens_per_second":{"decode":30.0,"prefill":40.0}}"#;
    store.apply(&parse_line(tp2.as_bytes()).unwrap());
    for kind in [
        ChartKind::Throughput,
        ChartKind::Latency,
        ChartKind::Cache,
        ChartKind::Scheduler,
    ] {
        let data = chart::prepare(&store, kind, 10_000, 6_000);
        assert_eq!(data.restarts, vec![5000.0], "{kind:?} restart marker");
    }
}
