// NInferMonitor
//
// This is free software; see the LICENSE.md file in the source distribution for precise wording.
//
// Copyright (C) 2026 Aleksey Sanin aleksey@aleksey.com. All Rights Reserved.

use std::collections::HashSet;

use crate::chart::format_time_ms;
use crate::parser::{EngineTiming, Materialization, SamplingParams, Speculative, ToolCallParse};
use crate::store::{RequestState, RequestStatus, Store};

/// Row colors (FR-5.3).
pub const COLOR_DONE: [u8; 3] = [0xd0, 0xd0, 0xd0];
pub const COLOR_SLOW: [u8; 3] = [0xff, 0xb7, 0x4d];
pub const COLOR_ERROR: [u8; 3] = [0xef, 0x53, 0x50];
pub const COLOR_INFLIGHT: [u8; 3] = [0x4f, 0xc3, 0xf7];

/// Table row geometry (px), shared with the Slint table layout (FR-5.1/FR-5.4).
pub const ROW_HEIGHT_PX: u32 = 32;
pub const ROW_SPACING_PX: u32 = 4;
pub const DETAIL_LINE_PX: u32 = 16;
pub const DETAIL_SPACING_PX: u32 = 4;
pub const DETAIL_PADDING_PX: u32 = 16;

/// Shown for missing values (FR-3.3 convention).
const NO_DATA: &str = "—";

/// One row of the request table (FR-5.1), with the inline detail payload
/// shown when the row is expanded (FR-5.4).
#[derive(Debug, Clone, PartialEq)]
pub struct TableRowData {
    pub id: String,
    pub time: String,
    pub model: String,
    pub prompt: String,
    pub compl: String,
    pub thinking: String,
    pub ttft: String,
    pub total: String,
    pub reason: String,
    pub status: String,
    pub color: [u8; 3],
    pub expanded: bool,
    pub detail: RequestDetail,
}

/// Inline detail content for a request (FR-5.4).
#[derive(Debug, Clone, PartialEq)]
pub struct RequestDetail {
    pub summary: String,
    pub sampling: String,
    pub engine: String,
    pub speculative: String,
    pub tool_call: String,
    pub prefix: String,
    /// KV-cache materialization diagnostics, empty when the request ran
    /// without KV pressure (the line is then hidden).
    pub materialization: String,
    pub error: String,
}

impl RequestDetail {
    /// An empty detail, used for collapsed rows so the (allocation-heavy)
    /// detail payload is only built for the rows that actually show it.
    fn empty() -> Self {
        Self {
            summary: String::new(),
            sampling: String::new(),
            engine: String::new(),
            speculative: String::new(),
            tool_call: String::new(),
            prefix: String::new(),
            materialization: String::new(),
            error: String::new(),
        }
    }
}

/// Build the table rows, newest first (FR-5.1/FR-5.3). `expanded_ids` marks
/// the rows whose inline detail is shown (FR-5.4).
pub fn table_rows(store: &Store, expanded_ids: &HashSet<u64>) -> Vec<TableRowData> {
    let p95 = p95_total(store);
    store
        .request_list()
        .map(|s| row_for(s, p95, expanded_ids.contains(&s.id)))
        .collect()
}

/// Toggle a request's expanded state (FR-5.4).
pub fn toggle_expanded(expanded_ids: &mut HashSet<u64>, id: u64) {
    if !expanded_ids.remove(&id) {
        expanded_ids.insert(id);
    }
}

/// Height of an expanded row's inline detail block (FR-5.4): the padding, the
/// fixed five lines, and the optional materialization and error lines.
pub fn detail_block_height(has_materialization: bool, has_error: bool) -> u32 {
    let lines = 5 + u32::from(has_materialization) + u32::from(has_error);
    DETAIL_PADDING_PX + lines * DETAIL_LINE_PX + (lines - 1) * DETAIL_SPACING_PX
}

/// Total scrollable height of the table (FR-5.1/FR-5.4): the rows, the
/// spacing between them, and the inline detail blocks of the expanded rows.
pub fn table_content_height(rows: &[TableRowData]) -> u32 {
    let n = rows.len() as u32;
    let mut height = n * ROW_HEIGHT_PX;
    if n > 0 {
        height += (n - 1) * ROW_SPACING_PX;
    }
    for r in rows {
        if r.expanded {
            height += detail_block_height(
                !r.detail.materialization.is_empty(),
                !r.detail.error.is_empty(),
            );
        }
    }
    height
}

fn detail_for(s: &RequestState) -> RequestDetail {
    let status = status_str(s.status);
    let summary = format!("Request {} · {}", s.id, status);
    let done = s.done.as_ref();
    let start_sampling = s
        .start
        .as_ref()
        .map(|st| format_sampling(&st.request.sampling));
    let sampling = done
        .map(|d| format_sampling(&d.request.sampling))
        .filter(|v| v.as_str() != NO_DATA)
        .or(start_sampling)
        .unwrap_or_else(|| NO_DATA.to_owned());
    RequestDetail {
        summary,
        engine: done
            .map(|d| format_engine(&d.engine_timing))
            .unwrap_or_else(|| NO_DATA.to_owned()),
        speculative: done
            .map(|d| format_speculative(&d.speculative))
            .unwrap_or_else(|| NO_DATA.to_owned()),
        tool_call: done
            .map(|d| format_tool_call(&d.result.tool_call_parse))
            .unwrap_or_else(|| NO_DATA.to_owned()),
        prefix: done
            .and_then(|d| d.result.prefix_reuse_path.as_deref())
            .map(str::to_owned)
            .unwrap_or_else(|| NO_DATA.to_owned()),
        materialization: done
            .map(|d| format_materialization(&d.materialization))
            .unwrap_or_default(),
        error: s
            .error
            .as_ref()
            .and_then(|e| e.error.message.as_deref())
            .map(str::to_owned)
            .unwrap_or_default(),
        sampling,
    }
}

/// p95 of completed requests' total time, nearest-rank (FR-5.3).
pub fn p95_total(store: &Store) -> Option<f64> {
    let values: Vec<f64> = store
        .request_list()
        .filter(|s| s.status == RequestStatus::Done)
        .filter_map(|s| s.done.as_ref().and_then(|d| d.timings_seconds.total))
        .collect();
    if values.is_empty() {
        return None;
    }
    let mut sorted = values;
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let rank = ((0.95 * sorted.len() as f64).ceil() as usize).clamp(1, sorted.len());
    Some(sorted[rank - 1])
}

fn row_for(s: &RequestState, p95: Option<f64>, expanded: bool) -> TableRowData {
    let done = s.done.as_ref();
    let prompt = done.and_then(|d| d.result.prompt_tokens);
    let compl = done.and_then(|d| d.result.completion_tokens);
    let thinking = done.and_then(|d| d.result.model_thinking_tokens);
    let ttft = done.and_then(|d| d.timings_seconds.ttft);
    let total = done.and_then(|d| d.timings_seconds.total);

    let slow = matches!(s.status, RequestStatus::Done)
        && total.is_some_and(|t| p95.is_some_and(|p| t > p));

    let color = match s.status {
        RequestStatus::Error => COLOR_ERROR,
        RequestStatus::InFlight => COLOR_INFLIGHT,
        RequestStatus::Done if slow => COLOR_SLOW,
        RequestStatus::Done => COLOR_DONE,
    };

    let reason = match s.status {
        RequestStatus::Error => s
            .error
            .as_ref()
            .and_then(|e| e.error.message.as_deref())
            .map(str::to_owned)
            .unwrap_or_else(|| NO_DATA.to_owned()),
        _ => done
            .and_then(|d| d.result.finish_reason.as_deref())
            .map(str::to_owned)
            .unwrap_or_else(|| NO_DATA.to_owned()),
    };

    let ts = s
        .start_time_ms()
        .or_else(|| done.and_then(|d| d.timestamp_unix_ms))
        .or_else(|| s.error.as_ref().and_then(|e| e.timestamp_unix_ms));

    TableRowData {
        id: s.id.to_string(),
        expanded,
        detail: if expanded {
            detail_for(s)
        } else {
            RequestDetail::empty()
        },
        time: ts
            .map(|t| format_time_ms(t as f64))
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| NO_DATA.to_owned()),
        model: s
            .model()
            .map(str::to_owned)
            .unwrap_or_else(|| NO_DATA.to_owned()),
        prompt: prompt
            .map(|v| v.to_string())
            .unwrap_or_else(|| NO_DATA.to_owned()),
        compl: compl
            .map(|v| v.to_string())
            .unwrap_or_else(|| NO_DATA.to_owned()),
        thinking: thinking
            .map(|v| v.to_string())
            .unwrap_or_else(|| NO_DATA.to_owned()),
        ttft: ttft.map(fmt_ms).unwrap_or_else(|| NO_DATA.to_owned()),
        total: total.map(fmt_s).unwrap_or_else(|| NO_DATA.to_owned()),
        reason,
        status: status_str(s.status),
        color,
    }
}

fn status_str(status: RequestStatus) -> String {
    match status {
        RequestStatus::InFlight => "in-flight".to_owned(),
        RequestStatus::Done => "done".to_owned(),
        RequestStatus::Error => "error".to_owned(),
    }
}

fn format_sampling(s: &SamplingParams) -> String {
    join_pairs(&[
        ("temperature", s.temperature.map(fmt_f64)),
        ("top_k", s.top_k.map(|v| v.to_string())),
        ("top_p", s.top_p.map(fmt_f64)),
        ("frequency_penalty", s.frequency_penalty.map(fmt_f64)),
        ("presence_penalty", s.presence_penalty.map(fmt_f64)),
        ("min_p", s.min_p.map(fmt_f64)),
        ("seed", s.seed.map(|v| v.to_string())),
    ])
}

fn format_engine(e: &EngineTiming) -> String {
    join_pairs(&[
        ("queue wait", e.queue_wait_seconds.map(fmt_ms)),
        ("device wait", e.device_wait_exposed_seconds.map(fmt_ms)),
        ("host exposed", e.host_exposed_seconds.total.map(fmt_ms)),
        ("decode rounds", e.decode.rounds.map(|v| v.to_string())),
        (
            "decode device wait",
            e.decode.device_wait_exposed_seconds.map(fmt_ms),
        ),
        ("decode host", e.decode.host_exposed_seconds.map(fmt_ms)),
        ("control units", e.units.control.map(|v| v.to_string())),
        ("prefill units", e.units.prefill.map(|v| v.to_string())),
    ])
}

fn format_speculative(s: &Speculative) -> String {
    // TODO(v0.1): `accepted_per_position` is parsed but not displayed.
    join_pairs(&[
        ("backend", s.backend.clone()),
        ("drafted", s.drafted_tokens.map(|v| v.to_string())),
        ("accepted", s.accepted_tokens.map(|v| v.to_string())),
        ("rounds", s.rounds.map(|v| v.to_string())),
        ("fallback steps", s.fallback_steps.map(|v| v.to_string())),
    ])
}

/// The materialization line: only the pressure-related fields, empty (line
/// hidden) when the request ran without KV pressure.
fn format_materialization(m: &Materialization) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(stop) = m.stop_reason.as_deref().filter(|s| *s != "no_pressure") {
        parts.push(format!("stop {stop}"));
    }
    if m.budget_exhausted == Some(true) {
        parts.push("budget exhausted".to_owned());
    }
    if let Some(n) = m.selected_degradation_units.filter(|&n| n > 0) {
        parts.push(format!("degraded {n}"));
    }
    if m.selected_maximal_fallback == Some(true) {
        parts.push("maximal fallback".to_owned());
    }
    if let Some(phase) = m.search_stop_phase.as_deref().filter(|s| *s != "none") {
        parts.push(format!("search {phase}"));
    }
    if m.search_boundary_limited == Some(true) {
        parts.push("boundary limited".to_owned());
    }
    parts.join(" · ")
}

fn format_tool_call(t: &ToolCallParse) -> String {
    join_pairs(&[
        ("structured", t.structured_call_count.map(|v| v.to_string())),
        ("fallback", t.fallback_reason.clone()),
        ("marker seen", t.marker_seen.map(|v| v.to_string())),
        (
            "empty args omitted",
            t.empty_arguments_omitted.map(|v| v.to_string()),
        ),
        (
            "schema mismatch",
            t.schema_mismatch_arguments.map(|v| v.to_string()),
        ),
    ])
}

fn join_pairs(pairs: &[(&str, Option<String>)]) -> String {
    let present: Vec<String> = pairs
        .iter()
        .filter_map(|(k, v)| v.as_ref().map(|v| format!("{k} {v}")))
        .collect();
    if present.is_empty() {
        NO_DATA.to_owned()
    } else {
        present.join(" · ")
    }
}

fn fmt_ms(seconds: f64) -> String {
    format!("{} ms", (seconds * 1000.0).round() as u64)
}

fn fmt_s(seconds: f64) -> String {
    format!("{seconds:.1} s")
}

fn fmt_f64(value: f64) -> String {
    format!("{value:.3}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{ParsedEvent, parse_line};
    use std::collections::HashSet;

    fn start_line(ts: u64, id: u64) -> ParsedEvent {
        let raw = format!(
            r#"{{"artifact_type":"ninfer_serve_request_log","schema_version":20,"server_instance_id":"s","timestamp_unix_ms":{ts},"event":"request_start","request":{{"request_id":{id},"model":"m","sampling":{{"temperature":0.1,"top_k":20,"top_p":0.95,"seed":7}}}}}}"#
        );
        parse_line(raw.as_bytes()).unwrap()
    }

    fn done_materialized_line(ts: u64, id: u64) -> ParsedEvent {
        let raw = format!(
            r#"{{"artifact_type":"ninfer_serve_request_log","schema_version":21,"server_instance_id":"s","timestamp_unix_ms":{ts},"event":"request_done","request":{{"request_id":{id}}},"result":{{"finish_reason":"stop_token"}},"timings_seconds":{{"total":1.5}},"materialization":{{"stop_reason":"time_budget","budget_exhausted":true,"selected_degradation_units":2,"selected_maximal_fallback":true,"search_stop_phase":"refinement","search_boundary_limited":true}}}}"#
        );
        parse_line(raw.as_bytes()).unwrap()
    }

    fn done_line(ts: u64, id: u64, total: f64) -> ParsedEvent {
        let raw = format!(
            r#"{{"artifact_type":"ninfer_serve_request_log","schema_version":20,"server_instance_id":"s","timestamp_unix_ms":{ts},"event":"request_done","request":{{"request_id":{id}}},"result":{{"prompt_tokens":100,"completion_tokens":50,"model_thinking_tokens":5,"finish_reason":"stop_token","prefix_reuse_path":"root","tool_call_parse":{{"structured_call_count":1,"fallback_reason":"none","marker_seen":true}}}},"timings_seconds":{{"ttft":0.5,"total":{total}}},"engine_timing":{{"queue_wait_seconds":0.001,"decode":{{"rounds":47}},"units":{{"control":0,"prefill":3}}}},"speculative":{{"backend":"mtp","drafted_tokens":141,"accepted_tokens":109,"rounds":47}}}}"#
        );
        parse_line(raw.as_bytes()).unwrap()
    }

    fn error_line(ts: u64, id: u64) -> ParsedEvent {
        let raw = format!(
            r#"{{"artifact_type":"ninfer_serve_request_log","schema_version":20,"server_instance_id":"s","timestamp_unix_ms":{ts},"event":"request_error","request":{{"request_id":{id}}},"error":{{"message":"client disconnected"}}}}"#
        );
        parse_line(raw.as_bytes()).unwrap()
    }

    #[test]
    fn empty_store_has_no_rows() {
        let store = Store::new();
        assert!(table_rows(&store, &HashSet::new()).is_empty());
        assert!(p95_total(&store).is_none());
        assert!(store.request(1).is_none());
    }

    #[test]
    fn rows_are_newest_first_with_values() {
        let mut store = Store::new();
        store.apply(&start_line(1_000, 1));
        store.apply(&done_line(2_000, 1, 1.5));
        store.apply(&start_line(3_000, 2));
        store.apply(&done_line(4_000, 2, 2.5));
        let rows = table_rows(&store, &HashSet::new());
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, "2");
        assert_eq!(rows[1].id, "1");
        assert_eq!(rows[0].prompt, "100");
        assert_eq!(rows[0].compl, "50");
        assert_eq!(rows[0].thinking, "5");
        assert_eq!(rows[0].ttft, "500 ms");
        assert_eq!(rows[0].total, "2.5 s");
        assert_eq!(rows[0].reason, "stop_token");
        assert_eq!(rows[0].status, "done");
        assert_eq!(rows[0].color, COLOR_DONE);
        assert_eq!(rows[0].model, "m");
    }

    #[test]
    fn inflight_row_shows_inflight_status() {
        let mut store = Store::new();
        store.apply(&start_line(1_000, 1));
        let rows = table_rows(&store, &HashSet::new());
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, "in-flight");
        assert_eq!(rows[0].color, COLOR_INFLIGHT);
        assert_eq!(rows[0].prompt, NO_DATA);
        assert_eq!(rows[0].ttft, NO_DATA);
    }

    #[test]
    fn row_transitions_inflight_to_done() {
        let mut store = Store::new();
        store.apply(&start_line(1_000, 1));
        let rows = table_rows(&store, &HashSet::new());
        assert_eq!(rows[0].status, "in-flight");
        assert_eq!(rows[0].color, COLOR_INFLIGHT);
        assert_eq!(rows[0].total, NO_DATA);
        assert_eq!(rows[0].reason, NO_DATA);

        store.apply(&done_line(2_000, 1, 1.5));
        let rows = table_rows(&store, &HashSet::new());
        assert_eq!(rows[0].status, "done");
        assert_eq!(rows[0].color, COLOR_DONE);
        assert_eq!(rows[0].total, "1.5 s");
        assert_eq!(rows[0].ttft, "500 ms");
        assert_eq!(rows[0].prompt, "100");
        assert_eq!(rows[0].reason, "stop_token");
    }

    #[test]
    fn error_row_shows_error_message_and_color() {
        let mut store = Store::new();
        store.apply(&start_line(1_000, 1));
        store.apply(&error_line(2_000, 1));
        let rows = table_rows(&store, &HashSet::new());
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, "error");
        assert_eq!(rows[0].color, COLOR_ERROR);
        assert_eq!(rows[0].reason, "client disconnected");
    }

    #[test]
    fn slow_row_is_flagged_above_p95() {
        let mut store = Store::new();
        for id in 1..=20u64 {
            store.apply(&start_line(id * 1_000, id));
            store.apply(&done_line(id * 1_000 + 500, id, 1.0));
        }
        // One outlier far above the rest.
        store.apply(&start_line(21_000, 21));
        store.apply(&done_line(22_000, 21, 100.0));
        let rows = table_rows(&store, &HashSet::new());
        let outlier = rows.iter().find(|r| r.id == "21").unwrap();
        assert_eq!(outlier.color, COLOR_SLOW);
        let normal = rows.iter().find(|r| r.id == "1").unwrap();
        assert_eq!(normal.color, COLOR_DONE);
    }

    #[test]
    fn p95_nearest_rank() {
        let mut store = Store::new();
        for id in 1..=20u64 {
            store.apply(&start_line(id, id));
            store.apply(&done_line(id + 1, id, id as f64));
        }
        // Totals 1..=20; nearest-rank p95 = ceil(0.95*20)=19th = 19.0.
        assert_eq!(p95_total(&store), Some(19.0));
    }

    #[test]
    fn detail_shows_done_payload() {
        let mut store = Store::new();
        store.apply(&start_line(1_000, 1));
        store.apply(&done_line(2_000, 1, 1.5));
        let d = detail_for(store.request(1).unwrap());
        assert_eq!(d.summary, "Request 1 · done");
        assert!(d.sampling.contains("temperature 0.100"));
        assert!(d.sampling.contains("top_k 20"));
        assert!(d.sampling.contains("seed 7"));
        assert!(d.engine.contains("decode rounds 47"));
        assert!(d.engine.contains("prefill units 3"));
        assert!(d.speculative.contains("backend mtp"));
        assert!(d.speculative.contains("accepted 109"));
        assert!(d.tool_call.contains("structured 1"));
        assert_eq!(d.prefix, "root");
        assert!(d.error.is_empty());
    }

    #[test]
    fn detail_shows_error_message() {
        let mut store = Store::new();
        store.apply(&start_line(1_000, 1));
        store.apply(&error_line(2_000, 1));
        let d = detail_for(store.request(1).unwrap());
        assert_eq!(d.summary, "Request 1 · error");
        assert_eq!(d.error, "client disconnected");
        assert_eq!(d.engine, NO_DATA);
    }

    #[test]
    fn detail_sampling_prefers_done_over_start() {
        let mut store = Store::new();
        store.apply(&start_line(1_000, 1));
        let raw = r#"{"artifact_type":"ninfer_serve_request_log","schema_version":20,"server_instance_id":"s","timestamp_unix_ms":2000,"event":"request_done","request":{"request_id":1,"sampling":{"temperature":0.7,"top_k":40}},"result":{"finish_reason":"stop_token"},"timings_seconds":{"total":1.5}}"#;
        store.apply(&parse_line(raw.as_bytes()).unwrap());
        let d = detail_for(store.request(1).unwrap());
        assert!(d.sampling.contains("temperature 0.700"));
        assert!(d.sampling.contains("top_k 40"));
        assert!(!d.sampling.contains("seed 7"));
    }

    #[test]
    fn detail_sampling_falls_back_to_start_when_done_empty() {
        let mut store = Store::new();
        store.apply(&start_line(1_000, 1));
        store.apply(&done_line(2_000, 1, 1.5));
        let d = detail_for(store.request(1).unwrap());
        assert!(d.sampling.contains("temperature 0.100"));
        assert!(d.sampling.contains("seed 7"));
    }

    #[test]
    fn detail_missing_request_is_none() {
        let store = Store::new();
        assert!(store.request(99).is_none());
    }

    #[test]
    fn toggle_expanded_adds_removes_and_keeps_others() {
        let mut ids = HashSet::new();
        toggle_expanded(&mut ids, 1);
        assert!(ids.contains(&1));
        toggle_expanded(&mut ids, 2);
        assert!(ids.contains(&1) && ids.contains(&2));
        toggle_expanded(&mut ids, 1);
        assert!(!ids.contains(&1) && ids.contains(&2));
    }

    #[test]
    fn table_rows_marks_expanded_rows() {
        let mut store = Store::new();
        store.apply(&start_line(1_000, 1));
        store.apply(&done_line(2_000, 1, 1.5));
        store.apply(&start_line(3_000, 2));
        store.apply(&done_line(4_000, 2, 2.5));
        let mut ids = HashSet::new();
        ids.insert(1);
        let rows = table_rows(&store, &ids);
        let r1 = rows.iter().find(|r| r.id == "1").unwrap();
        let r2 = rows.iter().find(|r| r.id == "2").unwrap();
        assert!(r1.expanded);
        assert!(!r2.expanded);
    }

    #[test]
    fn detail_shows_materialization_when_under_pressure() {
        let mut store = Store::new();
        store.apply(&start_line(1_000, 1));
        store.apply(&done_materialized_line(2_000, 1));
        let d = detail_for(store.request(1).unwrap());
        assert_eq!(
            d.materialization,
            "stop time_budget · budget exhausted · degraded 2 · maximal fallback \
             · search refinement · boundary limited"
        );
    }

    #[test]
    fn detail_hides_materialization_without_pressure() {
        let mut store = Store::new();
        store.apply(&start_line(1_000, 1));
        store.apply(&done_line(2_000, 1, 1.5));
        let d = detail_for(store.request(1).unwrap());
        assert!(d.materialization.is_empty());
    }

    #[test]
    fn detail_block_height_with_optional_lines() {
        assert_eq!(detail_block_height(false, false), 112);
        assert_eq!(detail_block_height(true, false), 132);
        assert_eq!(detail_block_height(false, true), 132);
        assert_eq!(detail_block_height(true, true), 152);
    }

    #[test]
    fn table_content_height_sums_rows_and_expanded_details() {
        let mut store = Store::new();
        store.apply(&start_line(1_000, 1));
        store.apply(&done_line(2_000, 1, 1.5));
        store.apply(&start_line(3_000, 2));
        store.apply(&done_line(4_000, 2, 2.5));
        let collapsed = table_rows(&store, &HashSet::new());
        assert_eq!(table_content_height(&collapsed), 2 * 32 + 4);
        let mut ids = HashSet::new();
        ids.insert(1);
        let expanded = table_rows(&store, &ids);
        assert_eq!(
            table_content_height(&expanded),
            2 * 32 + 4 + detail_block_height(false, false)
        );
    }
}
