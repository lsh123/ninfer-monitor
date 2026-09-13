// NInferMonitor
//
// This is free software; see the LICENSE.md file in the source distribution for precise wording.
//
// Copyright (C) 2026 Aleksey Sanin aleksey@aleksey.com. All Rights Reserved.

use std::path::{Path, PathBuf};

use ninfer_monitor::kpi::{self, Trend};
use ninfer_monitor::parser::parse_line;
use ninfer_monitor::store::Store;

fn sample_log_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/server.requests.jsonl")
}

fn feed_sample_log() -> Store {
    Store::load_jsonl(sample_log_path()).unwrap()
}

#[test]
fn sample_log_kpi_values_match_hand_computed() {
    let store = feed_sample_log();
    let k = kpi::snapshot(&store);
    // Last `throughput` event: decode 123.89240590071061, prefill 0.0.
    assert_eq!(k.decode.value, "123.9");
    assert_eq!(k.prefill.value, "0.0");
    // Last scheduler state is all zeros.
    assert_eq!(k.active.value, "0");
    assert_eq!(k.waiting.value, "0");
    // Last 60 s of the log (anchor 1789304713554): 11 completed requests,
    // mean TTFT 0.4456230272727273 s, mean decode latency/token
    // 0.007278349731205014 s, hit rate 1714827/1720108, spec acceptance
    // 6424/7764.
    assert_eq!(k.ttft.value, "446");
    assert_eq!(k.decode_latency.value, "7.3");
    assert_eq!(k.cache_hit.value, "99.7");
    assert_eq!(k.spec_accept.value, "82.7");
}

#[test]
fn sample_log_kpi_trends_match_hand_computed() {
    let store = feed_sample_log();
    let k = kpi::snapshot(&store);
    // Previous 60 s window [1789304593554, 1789304653554): mean decode
    // 135.76981534743467, mean prefill 8.065680443507889, mean TTFT
    // 0.46802099999999996, mean decode latency/token 0.007170840117364655,
    // hit rate 140978/141016, spec acceptance 5263/6711, 0 errors.
    assert_eq!(k.decode.trend, Trend::Down);
    assert_eq!(k.prefill.trend, Trend::Down);
    assert_eq!(k.ttft.trend, Trend::Down);
    assert_eq!(k.decode_latency.trend, Trend::Up);
    assert_eq!(k.cache_hit.trend, Trend::Down);
    assert_eq!(k.spec_accept.trend, Trend::Up);
    // Active is always flat (FR-3.2); mean waiting 0 vs. latest 0 -> Flat.
    assert_eq!(k.active.trend, Trend::Flat);
    assert_eq!(k.waiting.trend, Trend::Flat);
}

#[test]
fn appended_throughput_line_updates_kpis() {
    let mut store = feed_sample_log();
    let before = kpi::snapshot(&store);
    assert_eq!(before.decode.value, "123.9");
    assert_eq!(before.prefill.value, "0.0");

    let raw = r#"{"artifact_type":"ninfer_serve_request_log","schema_version":20,"server_instance_id":"serve-27872-1789281769048204","timestamp_unix_ms":1789304720000,"event":"throughput","throughput_tokens_per_second":{"decode":200.0,"prefill":50.0},"scheduler":{"running":1,"prefilling":0,"waiting":2}}"#;
    store.apply(&parse_line(raw.as_bytes()).unwrap());

    let after = kpi::snapshot(&store);
    assert_eq!(after.decode.value, "200.0");
    assert_eq!(after.prefill.value, "50.0");
    assert_eq!(after.active.value, "1");
    assert_eq!(after.waiting.value, "2");
    assert_eq!(after.ttft.value, "446");
    // Active is always flat (FR-3.2); mean waiting 0 vs. latest 2 -> Up.
    assert_eq!(after.active.trend, Trend::Flat);
    assert_eq!(after.waiting.trend, Trend::Up);
}

#[test]
fn appended_request_done_updates_windowed_kpis() {
    let mut store = feed_sample_log();
    let before = kpi::snapshot(&store);
    assert_eq!(before.ttft.value, "446");

    let raw = r#"{"artifact_type":"ninfer_serve_request_log","schema_version":20,"server_instance_id":"serve-27872-1789281769048204","timestamp_unix_ms":1789304720000,"event":"request_done","request":{"request_id":654},"result":{"prompt_tokens":1000,"completion_tokens":100,"prefix_cache_hit_tokens":0},"timings_seconds":{"ttft":1.0,"decode":2.0,"total":3.0},"speculative":{"drafted_tokens":100,"accepted_tokens":50}}"#;
    store.apply(&parse_line(raw.as_bytes()).unwrap());

    let after = kpi::snapshot(&store);
    // 12 requests in the window: (4.9018533 + 1.0) / 12
    // = 0.4918211083333333 s -> 492 ms; hit rate 1714827/1721108;
    // spec acceptance 6474/7864.
    assert_eq!(after.ttft.value, "492");
    assert_eq!(after.cache_hit.value, "99.6");
    assert_eq!(after.spec_accept.value, "82.3");
}
