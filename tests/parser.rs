// NInferMonitor
//
// This is free software; see the LICENSE.md file in the source distribution for precise wording.
//
// Copyright (C) 2026 Aleksey Sanin aleksey@aleksey.com. All Rights Reserved.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ninfer_monitor::parser::{ParsedEvent, parse_line};

fn sample_log_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/server.requests.jsonl")
}

fn parse_all() -> (Vec<ParsedEvent>, usize) {
    let data = fs::read_to_string(sample_log_path()).unwrap();
    let mut events = Vec::new();
    let mut skipped = 0;
    for line in data.lines() {
        match parse_line(line.as_bytes()) {
            Ok(event) => events.push(event),
            Err(_) => skipped += 1,
        }
    }
    (events, skipped)
}

fn assert_close(actual: Option<f64>, expected: f64) {
    let actual = actual.expect("value present");
    assert!(
        (actual - expected).abs() < 1e-9,
        "expected {expected}, got {actual}"
    );
}

#[test]
fn sample_log_every_line_parses_with_expected_counts() {
    let (events, skipped) = parse_all();
    assert_eq!(skipped, 0);
    assert_eq!(events.len(), 120);

    let server_start = events
        .iter()
        .filter(|e| matches!(e, ParsedEvent::ServerStart(_)))
        .count();
    let request_start = events
        .iter()
        .filter(|e| matches!(e, ParsedEvent::RequestStart(_)))
        .count();
    let request_done = events
        .iter()
        .filter(|e| matches!(e, ParsedEvent::RequestDone(_)))
        .count();
    let throughput = events
        .iter()
        .filter(|e| matches!(e, ParsedEvent::Throughput(_)))
        .count();
    let request_error = events
        .iter()
        .filter(|e| matches!(e, ParsedEvent::RequestError(_)))
        .count();
    let unknown = events
        .iter()
        .filter(|e| matches!(e, ParsedEvent::Unknown { .. }))
        .count();

    assert_eq!(server_start, 1);
    assert_eq!(request_start, 34);
    assert_eq!(request_done, 32);
    assert_eq!(throughput, 51);
    assert_eq!(request_error, 2);
    assert_eq!(unknown, 0);
}

#[test]
fn sample_log_request_one_spot_check() {
    let (events, _) = parse_all();
    let done = events
        .iter()
        .find_map(|e| match e {
            ParsedEvent::RequestDone(d) if d.request.request_id == Some(1) => Some(d),
            _ => None,
        })
        .expect("request 1 request_done");

    assert_eq!(done.result.prompt_tokens, Some(645));
    assert_eq!(done.result.completion_tokens, Some(155));
    assert_close(done.timings_seconds.ttft, 0.4219088);
    assert_eq!(done.speculative.accepted_tokens, Some(109));
    assert_eq!(done.speculative.drafted_tokens, Some(141));
    assert_eq!(done.result.finish_reason.as_deref(), Some("stop_token"));
}

#[test]
fn sample_log_first_and_last_throughput_values() {
    let (events, _) = parse_all();
    let tps: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            ParsedEvent::Throughput(t) => Some(t),
            _ => None,
        })
        .collect();
    assert_eq!(tps.len(), 51);

    let first = &tps[0];
    assert_close(first.throughput_tokens_per_second.decode, 53.71620426468579);
    assert_close(
        first.throughput_tokens_per_second.prefill,
        2461.725450667429,
    );

    let last = tps.last().unwrap();
    assert_close(last.throughput_tokens_per_second.decode, 123.89240590071061);
    assert_eq!(last.throughput_tokens_per_second.prefill, Some(0.0));
}

#[test]
fn sample_log_server_start_spot_check() {
    let (events, _) = parse_all();
    let ss = events
        .iter()
        .find_map(|e| match e {
            ParsedEvent::ServerStart(s) => Some(s),
            _ => None,
        })
        .expect("server_start");

    assert_eq!(
        ss.environment.gpu_name.as_deref(),
        Some("NVIDIA GeForce RTX 5090")
    );
    assert_eq!(ss.engine.kv_capacity, Some(240000));
    assert_eq!(ss.engine.max_context, Some(240000));
    assert_eq!(ss.engine.max_concurrency, Some(2));
    assert_eq!(ss.engine.kv_cache.as_deref(), Some("fp8-e4m3-row256"));
    assert_eq!(ss.artifact.name.as_deref(), Some(""));
    assert_eq!(ss.artifact.architecture.as_deref(), Some("qwen3_8_27b"));
    assert_eq!(ss.artifact.formats, vec!["nvfp4"]);
    assert_eq!(ss.artifact.device_object_count, Some(673));
    assert_eq!(ss.artifact.host_object_count, Some(6));
    assert_eq!(
        ss.server.public_model_id.as_deref(),
        Some("qwen3.8-27b-nvfp4")
    );
    assert_eq!(ss.memory.available_after_weights_bytes, Some(11310989312));
    assert_eq!(ss.memory.host_kv_capacity_bytes, Some(17179869184));
}

#[test]
fn sample_log_error_messages_and_finish_reasons() {
    let (events, _) = parse_all();
    let errors: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            ParsedEvent::RequestError(e) => Some(e),
            _ => None,
        })
        .collect();
    assert_eq!(errors.len(), 2);
    for e in &errors {
        assert!(
            matches!(
                e.error.message.as_deref(),
                Some(
                    "client disconnected" | "inference request expired while waiting for admission"
                )
            ),
            "unexpected error message: {:?}",
            e.error.message
        );
    }

    let done: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            ParsedEvent::RequestDone(d) => Some(d),
            _ => None,
        })
        .collect();
    assert_eq!(done.len(), 32);
    for d in &done {
        assert!(
            matches!(
                d.result.finish_reason.as_deref(),
                Some("stop_token" | "cancelled")
            ),
            "unexpected finish reason: {:?}",
            d.result.finish_reason
        );
    }
}

#[test]
fn sample_log_envelopes_are_consistent() {
    let (events, skipped) = parse_all();
    assert_eq!(skipped, 0);
    for e in &events {
        assert_eq!(e.schema_version(), Some(21));
        assert!(!e.schema_too_new());
        assert_eq!(e.server_instance_id(), Some("serve-27872-1789281769048204"));
        assert!(e.timestamp_unix_ms().is_some());
    }
}

#[test]
fn sample_log_request_lifecycle_closes() {
    let (events, _) = parse_all();
    let starts: HashSet<u64> = events
        .iter()
        .filter_map(|e| match e {
            ParsedEvent::RequestStart(s) => s.request.request_id,
            _ => None,
        })
        .collect();
    let closes: HashSet<u64> = events
        .iter()
        .flat_map(|e| match e {
            ParsedEvent::RequestDone(d) => [d.request.request_id],
            ParsedEvent::RequestError(r) => [r.request.request_id],
            _ => [None],
        })
        .flatten()
        .collect();

    assert_eq!(starts.len(), 34);
    assert_eq!(closes.len(), 34);
    assert_eq!(starts, closes);
}

#[test]
fn sample_log_backfill_parses_fast() {
    let start = Instant::now();
    let (events, skipped) = parse_all();
    let elapsed = start.elapsed();

    assert_eq!(events.len(), 120);
    assert_eq!(skipped, 0);
    assert!(
        elapsed < Duration::from_secs(5),
        "parsing the sample log took {elapsed:?}"
    );
}
