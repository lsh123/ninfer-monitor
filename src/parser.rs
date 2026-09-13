// NInferMonitor
//
// This is free software; see the LICENSE.md file in the source distribution for precise wording.
//
// Copyright (C) 2026 Aleksey Sanin aleksey@aleksey.com. All Rights Reserved.

//! Trust model for the log input. The tailled `server.requests.jsonl` is
//! parsed defensively: it is an untrusted, machine-generated input, and the
//! parser must never crash or panic on malformed data. Unknown events and
//! fields are ignored, missing or wrong-typed fields fall back to defaults,
//! and malformed lines are skipped and counted (FR-2.3/FR-2.4) rather than
//! fatal. All values cross into the app as plain owned data (no `unsafe`, no
//! deserialization into `Deserialize`-driven code execution), so a hostile or
//! corrupt log degrades to missing data — it cannot drive the app into an
//! invalid state.
//!
//! The one user-supplied path (the opened log file) and the config file are
//! the only file I/O the app performs, and config writes are atomic
//! (temp file + rename) with malformed files repaired to defaults at startup.

use serde::Deserialize;
use serde_json::Value;
use thiserror::Error;

pub const SUPPORTED_SCHEMA_VERSION: u32 = 20;

#[derive(Debug, Error)]
pub enum ParseError {
    #[error("invalid JSON: {0}")]
    InvalidJson(#[from] serde_json::Error),
    #[error("line is not a JSON object")]
    NotAnObject,
    #[error("missing required field `{0}`")]
    MissingField(&'static str),
    #[error("field `{0}` has the wrong type")]
    WrongType(&'static str),
}

#[derive(Debug, Clone)]
pub enum ParsedEvent {
    ServerStart(ServerStartEvent),
    RequestStart(RequestStartEvent),
    RequestDone(RequestDoneEvent),
    Throughput(ThroughputEvent),
    RequestError(RequestErrorEvent),
    Unknown {
        event: String,
        schema_version: Option<u32>,
        timestamp_unix_ms: Option<u64>,
        server_instance_id: Option<String>,
    },
}

impl ParsedEvent {
    pub fn schema_version(&self) -> Option<u32> {
        match self {
            ParsedEvent::ServerStart(e) => e.schema_version,
            ParsedEvent::RequestStart(e) => e.schema_version,
            ParsedEvent::RequestDone(e) => e.schema_version,
            ParsedEvent::Throughput(e) => e.schema_version,
            ParsedEvent::RequestError(e) => e.schema_version,
            ParsedEvent::Unknown { schema_version, .. } => *schema_version,
        }
    }

    pub fn schema_too_new(&self) -> bool {
        self.schema_version()
            .is_some_and(|v| v > SUPPORTED_SCHEMA_VERSION)
    }

    pub fn timestamp_unix_ms(&self) -> Option<u64> {
        match self {
            ParsedEvent::ServerStart(e) => e.timestamp_unix_ms,
            ParsedEvent::RequestStart(e) => e.timestamp_unix_ms,
            ParsedEvent::RequestDone(e) => e.timestamp_unix_ms,
            ParsedEvent::Throughput(e) => e.timestamp_unix_ms,
            ParsedEvent::RequestError(e) => e.timestamp_unix_ms,
            ParsedEvent::Unknown {
                timestamp_unix_ms, ..
            } => *timestamp_unix_ms,
        }
    }

    pub fn server_instance_id(&self) -> Option<&str> {
        match self {
            ParsedEvent::ServerStart(e) => e.server_instance_id.as_deref(),
            ParsedEvent::RequestStart(e) => e.server_instance_id.as_deref(),
            ParsedEvent::RequestDone(e) => e.server_instance_id.as_deref(),
            ParsedEvent::Throughput(e) => e.server_instance_id.as_deref(),
            ParsedEvent::RequestError(e) => e.server_instance_id.as_deref(),
            ParsedEvent::Unknown {
                server_instance_id, ..
            } => server_instance_id.as_deref(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ServerStartEvent {
    pub artifact_type: Option<String>,
    pub schema_version: Option<u32>,
    pub server_instance_id: Option<String>,
    pub timestamp_unix_ms: Option<u64>,
    pub argv: Vec<String>,
    pub artifact: Artifact,
    pub engine: Engine,
    pub environment: Environment,
    pub memory: Memory,
    pub server: ServerInfo,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Artifact {
    pub target: Option<String>,
    pub weights_id: Option<String>,
    pub size_bytes: Option<u64>,
    pub load_seconds: Option<f64>,
    pub upload_seconds: Option<f64>,
    pub tensor_count: Option<u64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Engine {
    pub model_id: Option<String>,
    pub public_model_id: Option<String>,
    pub kv_cache: Option<String>,
    pub kv_capacity: Option<u64>,
    pub max_context: Option<u64>,
    pub max_concurrency: Option<u64>,
    pub prefill_chunk: Option<u64>,
    pub speculative_backend: Option<String>,
    pub cuda_graph: Option<bool>,
    pub prefix_reuse: Option<bool>,
    pub log_stats_interval_ms: Option<u64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Environment {
    pub gpu_name: Option<String>,
    pub compute_capability_major: Option<u32>,
    pub compute_capability_minor: Option<u32>,
    pub cuda_compile_version: Option<String>,
    pub cuda_driver_version: Option<String>,
    pub cuda_runtime_version: Option<String>,
    pub total_device_memory_bytes: Option<u64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Memory {
    pub weights: MemorySegment,
    pub sequence: MemorySegment,
    pub workspace: MemorySegment,
    pub available_after_weights_bytes: Option<u64>,
    pub host_kv_capacity_bytes: Option<u64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct MemorySegment {
    pub capacity_bytes: Option<u64>,
    pub used_bytes: Option<u64>,
    pub peak_used_bytes: Option<u64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ServerInfo {
    pub host: Option<String>,
    pub port: Option<u32>,
    pub public_model_id: Option<String>,
    pub request_log_jsonl: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct RequestStartEvent {
    pub artifact_type: Option<String>,
    pub schema_version: Option<u32>,
    pub server_instance_id: Option<String>,
    pub timestamp_unix_ms: Option<u64>,
    pub request: RequestInfo,
    pub preparation_seconds: PreparationSeconds,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct RequestInfo {
    pub request_id: Option<u64>,
    pub model: Option<String>,
    pub protocol: Option<String>,
    pub message_count: Option<u64>,
    pub tool_count: Option<u64>,
    pub stream: Option<bool>,
    pub sampling: SamplingParams,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct SamplingParams {
    pub temperature: Option<f64>,
    pub top_k: Option<u32>,
    pub top_p: Option<f64>,
    pub frequency_penalty: Option<f64>,
    pub presence_penalty: Option<f64>,
    pub min_p: Option<f64>,
    pub seed: Option<u64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PreparationSeconds {
    pub tokenize: Option<f64>,
    pub total: Option<f64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct RequestDoneEvent {
    pub artifact_type: Option<String>,
    pub schema_version: Option<u32>,
    pub server_instance_id: Option<String>,
    pub timestamp_unix_ms: Option<u64>,
    pub request: RequestInfo,
    pub result: ResultInfo,
    pub timings_seconds: TimingsSeconds,
    pub engine_timing: EngineTiming,
    pub speculative: Speculative,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ResultInfo {
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub model_thinking_tokens: Option<u64>,
    pub finish_reason: Option<String>,
    pub prefix_cache_hit_tokens: Option<u64>,
    pub prefix_reuse_path: Option<String>,
    pub tool_call_count: Option<u64>,
    pub tool_call_parse: ToolCallParse,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ToolCallParse {
    pub structured_call_count: Option<u64>,
    pub fallback_reason: Option<String>,
    pub marker_seen: Option<bool>,
    pub empty_arguments_omitted: Option<u64>,
    pub schema_mismatch_arguments: Option<u64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct TimingsSeconds {
    pub ttft: Option<f64>,
    pub prefill: Option<f64>,
    pub decode: Option<f64>,
    pub prepare: Option<f64>,
    pub total: Option<f64>,
    pub vision: Option<f64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct EngineTiming {
    pub queue_wait_seconds: Option<f64>,
    pub device_wait_exposed_seconds: Option<f64>,
    pub host_exposed_seconds: HostExposedSeconds,
    pub decode: EngineTimingDecode,
    pub units: EngineTimingUnits,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct HostExposedSeconds {
    pub engine_boundary: Option<f64>,
    pub engine_commit_output: Option<f64>,
    pub engine_maintenance: Option<f64>,
    pub program_post: Option<f64>,
    pub program_submit: Option<f64>,
    pub total: Option<f64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct EngineTimingDecode {
    pub rounds: Option<u64>,
    pub device_wait_exposed_seconds: Option<f64>,
    pub host_exposed_seconds: Option<f64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct EngineTimingUnits {
    pub control: Option<u64>,
    pub prefill: Option<u64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Speculative {
    pub backend: Option<String>,
    pub drafted_tokens: Option<u64>,
    pub accepted_tokens: Option<u64>,
    pub accepted_per_position: Vec<u64>,
    pub fallback_steps: Option<u64>,
    pub rounds: Option<u64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ThroughputEvent {
    pub artifact_type: Option<String>,
    pub schema_version: Option<u32>,
    pub server_instance_id: Option<String>,
    pub timestamp_unix_ms: Option<u64>,
    pub throughput_tokens_per_second: ThroughputTokensPerSecond,
    pub tokens: ThroughputTokens,
    pub decode_batch: DecodeBatch,
    pub scheduler: Scheduler,
    pub context_cache: ContextCache,
    pub host_work: HostWork,
    pub interval_seconds: Option<f64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ThroughputTokensPerSecond {
    pub decode: Option<f64>,
    pub prefill: Option<f64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ThroughputTokens {
    pub committed_decode: Option<u64>,
    pub computed_prefill: Option<u64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct DecodeBatch {
    pub rounds: Option<u64>,
    pub average_size: Option<f64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Scheduler {
    pub running: Option<u64>,
    pub prefilling: Option<u64>,
    pub waiting: Option<u64>,
    pub materializing: Option<u64>,
    pub decode_ready: Option<u64>,
    pub capture_pending: Option<u64>,
    pub terminal_pending: Option<u64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ContextCache {
    pub occupancy: Occupancy,
    pub pressure: Pressure,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Occupancy {
    pub device_main_kv_pages: Option<u64>,
    pub device_backend_kv_pages: Option<u64>,
    pub host_kv_bytes: Option<u64>,
    pub host_state_slots: Option<u64>,
    pub device_state_slots: Option<u64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Pressure {
    pub searches: Option<u64>,
    pub search_budget_exhaustions: Option<u64>,
    pub private_owners_evicted: Option<u64>,
    pub private_owners_degraded: Option<u64>,
    pub shared_owners_evicted: Option<u64>,
    pub shared_owners_degraded: Option<u64>,
    pub spill_pages: Option<u64>,
    pub partial_tail_cow_pages: Option<u64>,
    pub maximal_fallback_selections: Option<u64>,
    pub checkpoints_dropped: Option<u64>,
    pub historical_fork_hits: Option<u64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct HostWork {
    pub work_class_seconds: WorkClassSeconds,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct WorkClassSeconds {
    pub control_device_wait: Option<f64>,
    pub control_host: Option<f64>,
    pub decode_device_wait: Option<f64>,
    pub decode_host: Option<f64>,
    pub prefill_device_wait: Option<f64>,
    pub prefill_host: Option<f64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct RequestErrorEvent {
    pub artifact_type: Option<String>,
    pub schema_version: Option<u32>,
    pub server_instance_id: Option<String>,
    pub timestamp_unix_ms: Option<u64>,
    pub request: RequestInfo,
    pub error: ErrorInfo,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ErrorInfo {
    pub message: Option<String>,
}

pub fn parse_line(line: &[u8]) -> Result<ParsedEvent, ParseError> {
    let line = strip_bom(line);
    if matches!(
        line.iter().copied().find(|b| !b.is_ascii_whitespace()),
        Some(b) if b != b'{'
    ) {
        return Err(ParseError::NotAnObject);
    }
    let value: Value = serde_json::from_slice(line)?;
    let event_name = match value.get("event") {
        Some(v) => v
            .as_str()
            .map(str::to_owned)
            .ok_or(ParseError::WrongType("event"))?,
        None => return Err(ParseError::MissingField("event")),
    };
    let schema_version = value
        .get("schema_version")
        .and_then(|v| v.as_u64())
        .and_then(|v| u32::try_from(v).ok());
    let timestamp_unix_ms = value.get("timestamp_unix_ms").and_then(|v| v.as_u64());
    let server_instance_id = value
        .get("server_instance_id")
        .and_then(|v| v.as_str())
        .map(str::to_owned);
    match event_name.as_str() {
        "server_start" => parse_server_start(value),
        "request_start" => parse_request_start(value),
        "request_done" => parse_request_done(value),
        "throughput" => parse_throughput(value),
        "request_error" => parse_request_error(value),
        other => Ok(ParsedEvent::Unknown {
            event: other.to_owned(),
            schema_version,
            timestamp_unix_ms,
            server_instance_id,
        }),
    }
}

fn strip_bom(line: &[u8]) -> &[u8] {
    const BOM: [u8; 3] = [0xEF, 0xBB, 0xBF];
    line.strip_prefix(&BOM).unwrap_or(line)
}

fn validate_envelope(
    artifact_type: Option<&str>,
    schema_version: Option<u32>,
    server_instance_id: Option<&str>,
    timestamp_unix_ms: Option<u64>,
) -> Result<(), ParseError> {
    if artifact_type.is_none() {
        return Err(ParseError::MissingField("artifact_type"));
    }
    if schema_version.is_none() {
        return Err(ParseError::MissingField("schema_version"));
    }
    if server_instance_id.is_none() {
        return Err(ParseError::MissingField("server_instance_id"));
    }
    if timestamp_unix_ms.is_none() {
        return Err(ParseError::MissingField("timestamp_unix_ms"));
    }
    Ok(())
}

fn parse_server_start(value: Value) -> Result<ParsedEvent, ParseError> {
    let event: ServerStartEvent = serde_json::from_value(value)?;
    validate_envelope(
        event.artifact_type.as_deref(),
        event.schema_version,
        event.server_instance_id.as_deref(),
        event.timestamp_unix_ms,
    )?;
    Ok(ParsedEvent::ServerStart(event))
}

fn parse_request_start(value: Value) -> Result<ParsedEvent, ParseError> {
    let event: RequestStartEvent = serde_json::from_value(value)?;
    validate_envelope(
        event.artifact_type.as_deref(),
        event.schema_version,
        event.server_instance_id.as_deref(),
        event.timestamp_unix_ms,
    )?;
    if event.request.request_id.is_none() {
        return Err(ParseError::MissingField("request.request_id"));
    }
    Ok(ParsedEvent::RequestStart(event))
}

fn parse_request_done(value: Value) -> Result<ParsedEvent, ParseError> {
    let event: RequestDoneEvent = serde_json::from_value(value)?;
    validate_envelope(
        event.artifact_type.as_deref(),
        event.schema_version,
        event.server_instance_id.as_deref(),
        event.timestamp_unix_ms,
    )?;
    if event.request.request_id.is_none() {
        return Err(ParseError::MissingField("request.request_id"));
    }
    Ok(ParsedEvent::RequestDone(event))
}

fn parse_throughput(value: Value) -> Result<ParsedEvent, ParseError> {
    let event: ThroughputEvent = serde_json::from_value(value)?;
    validate_envelope(
        event.artifact_type.as_deref(),
        event.schema_version,
        event.server_instance_id.as_deref(),
        event.timestamp_unix_ms,
    )?;
    Ok(ParsedEvent::Throughput(event))
}

fn parse_request_error(value: Value) -> Result<ParsedEvent, ParseError> {
    let event: RequestErrorEvent = serde_json::from_value(value)?;
    validate_envelope(
        event.artifact_type.as_deref(),
        event.schema_version,
        event.server_instance_id.as_deref(),
        event.timestamp_unix_ms,
    )?;
    if event.request.request_id.is_none() {
        return Err(ParseError::MissingField("request.request_id"));
    }
    if event.error.message.is_none() {
        return Err(ParseError::MissingField("error.message"));
    }
    Ok(ParsedEvent::RequestError(event))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ENVELOPE: &str = r#"
        "artifact_type": "ninfer_serve_request_log",
        "schema_version": 20,
        "server_instance_id": "serve-test-1",
        "timestamp_unix_ms": 1789281778921
    "#;

    fn line(body: &str) -> Vec<u8> {
        format!("{{{ENVELOPE},\"event\":\"{body}\"}}").into_bytes()
    }

    #[test]
    fn malformed_json_returns_invalid_json_error() {
        let err = parse_line(b"{\"broken").unwrap_err();
        assert!(matches!(err, ParseError::InvalidJson(_)));
    }

    #[test]
    fn empty_line_returns_error() {
        assert!(parse_line(b"").is_err());
        assert!(parse_line(b"   \n").is_err());
    }

    #[test]
    fn non_object_json_returns_not_an_object() {
        assert!(matches!(
            parse_line(b"[1, 2, 3]"),
            Err(ParseError::NotAnObject)
        ));
        assert!(matches!(
            parse_line(b"\"hello\""),
            Err(ParseError::NotAnObject)
        ));
        assert!(matches!(parse_line(b"42"), Err(ParseError::NotAnObject)));
        assert!(matches!(parse_line(b"null"), Err(ParseError::NotAnObject)));
    }

    #[test]
    fn missing_event_field_returns_missing_field() {
        let err = parse_line(format!("{{{ENVELOPE}}}").as_bytes()).unwrap_err();
        assert!(matches!(err, ParseError::MissingField("event")));
    }

    #[test]
    fn non_string_event_field_returns_wrong_type() {
        let err = parse_line(format!("{{{ENVELOPE},\"event\":7}}").as_bytes()).unwrap_err();
        assert!(matches!(err, ParseError::WrongType("event")));
    }

    #[test]
    fn unknown_event_returns_unknown_variant() {
        let event = parse_line(&line("future_event")).unwrap();
        match &event {
            ParsedEvent::Unknown {
                event,
                schema_version,
                timestamp_unix_ms,
                server_instance_id,
            } => {
                assert_eq!(event, "future_event");
                assert_eq!(*schema_version, Some(20));
                assert_eq!(*timestamp_unix_ms, Some(1789281778921));
                assert_eq!(server_instance_id.as_deref(), Some("serve-test-1"));
            }
            other => panic!("expected Unknown, got {other:?}"),
        }
        assert_eq!(event.schema_version(), Some(20));
        assert!(!event.schema_too_new());
        assert_eq!(event.timestamp_unix_ms(), Some(1789281778921));
        assert_eq!(event.server_instance_id(), Some("serve-test-1"));
    }

    #[test]
    fn unknown_event_with_extra_fields_is_ignored() {
        let raw = format!(
            "{{{ENVELOPE},\"event\":\"future_event\",\"payload\":{{\"x\":1,\"y\":[2,3]}}}}"
        );
        assert!(matches!(
            parse_line(raw.as_bytes()).unwrap(),
            ParsedEvent::Unknown { .. }
        ));
    }

    #[test]
    fn missing_envelope_fields_are_rejected() {
        let cases = [
            (
                r#""schema_version":20,"server_instance_id":"s","timestamp_unix_ms":1"#,
                "artifact_type",
            ),
            (
                r#""artifact_type":"ninfer_serve_request_log","server_instance_id":"s","timestamp_unix_ms":1"#,
                "schema_version",
            ),
            (
                r#""artifact_type":"ninfer_serve_request_log","schema_version":20,"timestamp_unix_ms":1"#,
                "server_instance_id",
            ),
            (
                r#""artifact_type":"ninfer_serve_request_log","schema_version":20,"server_instance_id":"s""#,
                "timestamp_unix_ms",
            ),
        ];
        for (body, field) in cases {
            let raw = format!("{{{body},\"event\":\"throughput\"}}");
            let err = parse_line(raw.as_bytes()).unwrap_err();
            assert!(
                matches!(err, ParseError::MissingField(f) if f == field),
                "expected missing `{field}`, got {err:?}"
            );
        }
    }

    #[test]
    fn request_events_require_request_id() {
        for event in ["request_start", "request_done", "request_error"] {
            let raw =
                format!("{{{ENVELOPE},\"event\":\"{event}\",\"request\":{{\"model\":\"m\"}}}}");
            let err = parse_line(raw.as_bytes()).unwrap_err();
            assert!(
                matches!(err, ParseError::MissingField("request.request_id")),
                "{event}: expected missing request_id, got {err:?}"
            );
        }
    }

    #[test]
    fn request_error_requires_message() {
        let raw = format!(
            "{{{ENVELOPE},\"event\":\"request_error\",\"request\":{{\"request_id\":1}},\"error\":{{}}}}"
        );
        let err = parse_line(raw.as_bytes()).unwrap_err();
        assert!(matches!(err, ParseError::MissingField("error.message")));
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let raw = format!(
            "{{{ENVELOPE},\"event\":\"throughput\",\"brand_new\":{{\"x\":1}},\
             \"throughput_tokens_per_second\":{{\"decode\":1.5,\"prefill\":2.5,\"mystery\":3}}}}"
        );
        let ParsedEvent::Throughput(tp) = parse_line(raw.as_bytes()).unwrap() else {
            panic!("expected Throughput");
        };
        assert_eq!(tp.throughput_tokens_per_second.decode, Some(1.5));
        assert_eq!(tp.throughput_tokens_per_second.prefill, Some(2.5));
    }

    #[test]
    fn missing_optional_fields_default_to_none() {
        let ParsedEvent::Throughput(tp) = parse_line(&line("throughput")).unwrap() else {
            panic!("expected Throughput");
        };
        assert_eq!(tp.throughput_tokens_per_second.decode, None);
        assert_eq!(tp.tokens.committed_decode, None);
        assert_eq!(tp.decode_batch.rounds, None);
        assert_eq!(tp.scheduler.running, None);
        assert_eq!(tp.context_cache.occupancy.device_main_kv_pages, None);
        assert_eq!(tp.context_cache.pressure.searches, None);
        assert_eq!(tp.host_work.work_class_seconds.decode_host, None);
        assert_eq!(tp.interval_seconds, None);
    }

    #[test]
    fn newer_schema_version_parses_and_flags_too_new() {
        let raw = "{\"artifact_type\":\"ninfer_serve_request_log\",\"schema_version\":21,\
                   \"server_instance_id\":\"s\",\"timestamp_unix_ms\":1,\
                   \"event\":\"throughput\"}";
        let event = parse_line(raw.as_bytes()).unwrap();
        assert_eq!(event.schema_version(), Some(21));
        assert!(event.schema_too_new());
    }

    #[test]
    fn supported_schema_version_is_not_flagged() {
        let event = parse_line(&line("throughput")).unwrap();
        assert_eq!(event.schema_version(), Some(20));
        assert!(!event.schema_too_new());
    }

    #[test]
    fn utf8_bom_is_stripped() {
        let raw = line("throughput");
        let mut with_bom = vec![0xEF, 0xBB, 0xBF];
        with_bom.extend_from_slice(&raw);
        assert!(parse_line(&with_bom).is_ok());
    }

    #[test]
    fn trailing_whitespace_and_crlf_are_tolerated() {
        let raw = format!("{{{ENVELOPE},\"event\":\"throughput\"}}\r\n  ");
        assert!(parse_line(raw.as_bytes()).is_ok());
    }

    #[test]
    fn wrong_field_type_returns_error() {
        let raw = format!(
            "{{{ENVELOPE},\"event\":\"request_start\",\"request\":{{\"request_id\":\"abc\"}}}}"
        );
        assert!(parse_line(raw.as_bytes()).is_err());
        let raw = format!("{{{ENVELOPE},\"event\":\"throughput\",\"interval_seconds\":\"fast\"}}");
        assert!(parse_line(raw.as_bytes()).is_err());
    }

    #[test]
    fn long_line_parses() {
        let pad = "x".repeat(1_000_000);
        let raw = format!("{{{ENVELOPE},\"event\":\"throughput\",\"pad\":\"{pad}\"}}");
        assert!(parse_line(raw.as_bytes()).is_ok());
    }

    #[test]
    fn server_start_round_trip() {
        let raw = format!(
            r#"{{{ENVELOPE},"event":"server_start",
            "argv":["ninfer-serve","--port","8080"],
            "artifact":{{"target":"qwen3_8_27b","weights_id":"nvfp4","size_bytes":23719496192,"load_seconds":9.6972125,"upload_seconds":6.8405596,"tensor_count":673}},
            "engine":{{"kv_cache":"fp8-e4m3-row256","kv_capacity":240000,"max_context":240000,"max_concurrency":2,"prefill_chunk":2048,"speculative_backend":"mtp","cuda_graph":true,"prefix_reuse":true,"log_stats_interval_ms":5000}},
            "environment":{{"gpu_name":"NVIDIA GeForce RTX 5090","compute_capability_major":12,"compute_capability_minor":0,"cuda_driver_version":"13.4","cuda_runtime_version":"13.4","total_device_memory_bytes":34162016256}},
            "memory":{{"weights":{{"capacity_bytes":21183929344,"used_bytes":21183929344,"peak_used_bytes":21183929344}},"sequence":{{"capacity_bytes":9080226048,"used_bytes":9080226048,"peak_used_bytes":9080226048}},"workspace":{{"capacity_bytes":319963136,"used_bytes":0,"peak_used_bytes":319963136}},"available_after_weights_bytes":11310989312,"host_kv_capacity_bytes":17179869184}},
            "server":{{"host":"0.0.0.0","port":8080,"public_model_id":"qwen3.8-27b-nvfp4","request_log_jsonl":"d:/NInfer/logs/server.requests.jsonl"}}}}"#
        );
        let ParsedEvent::ServerStart(e) = parse_line(raw.as_bytes()).unwrap() else {
            panic!("expected ServerStart");
        };
        assert_eq!(e.argv, vec!["ninfer-serve", "--port", "8080"]);
        assert_eq!(e.artifact.target.as_deref(), Some("qwen3_8_27b"));
        assert_eq!(e.artifact.weights_id.as_deref(), Some("nvfp4"));
        assert_eq!(e.artifact.size_bytes, Some(23719496192));
        assert_eq!(e.artifact.tensor_count, Some(673));
        assert_eq!(e.engine.kv_capacity, Some(240000));
        assert_eq!(e.engine.max_context, Some(240000));
        assert_eq!(e.engine.max_concurrency, Some(2));
        assert_eq!(e.engine.cuda_graph, Some(true));
        assert_eq!(e.engine.log_stats_interval_ms, Some(5000));
        assert_eq!(
            e.environment.gpu_name.as_deref(),
            Some("NVIDIA GeForce RTX 5090")
        );
        assert_eq!(e.environment.compute_capability_major, Some(12));
        assert_eq!(e.environment.total_device_memory_bytes, Some(34162016256));
        assert_eq!(e.memory.weights.used_bytes, Some(21183929344));
        assert_eq!(e.memory.workspace.used_bytes, Some(0));
        assert_eq!(e.memory.available_after_weights_bytes, Some(11310989312));
        assert_eq!(e.server.port, Some(8080));
        assert_eq!(
            e.server.public_model_id.as_deref(),
            Some("qwen3.8-27b-nvfp4")
        );
        assert_eq!(e.timestamp_unix_ms, Some(1789281778921));
        assert_eq!(e.server_instance_id.as_deref(), Some("serve-test-1"));
    }

    #[test]
    fn request_start_round_trip() {
        let raw = format!(
            r#"{{{ENVELOPE},"event":"request_start",
            "request":{{"request_id":1,"model":"qwen3.8-27b-nvfp4","protocol":"openai_chat_completions","message_count":3,"tool_count":0,"stream":true,"sampling":{{"temperature":0.1,"top_k":20,"top_p":0.95,"seed":7110312667474542792}}}},
            "preparation_seconds":{{"tokenize":0.0005802,"total":0.0005981}}}}"#
        );
        let ParsedEvent::RequestStart(e) = parse_line(raw.as_bytes()).unwrap() else {
            panic!("expected RequestStart");
        };
        assert_eq!(e.request.request_id, Some(1));
        assert_eq!(e.request.model.as_deref(), Some("qwen3.8-27b-nvfp4"));
        assert_eq!(e.request.message_count, Some(3));
        assert_eq!(e.request.stream, Some(true));
        assert_eq!(e.request.sampling.seed, Some(7110312667474542792));
        assert_eq!(e.request.sampling.top_k, Some(20));
        assert_eq!(e.preparation_seconds.tokenize, Some(0.0005802));
        assert_eq!(e.preparation_seconds.total, Some(0.0005981));
    }

    #[test]
    fn seed_above_i64_max_parses() {
        let raw = format!(
            r#"{{{ENVELOPE},"event":"request_start",
            "request":{{"request_id":2,"sampling":{{"seed":18436630210268143187}}}}}}"#
        );
        let ParsedEvent::RequestStart(e) = parse_line(raw.as_bytes()).unwrap() else {
            panic!("expected RequestStart");
        };
        assert_eq!(e.request.sampling.seed, Some(18436630210268143187));
    }

    #[test]
    fn request_done_round_trip() {
        let raw = format!(
            r#"{{{ENVELOPE},"event":"request_done",
            "request":{{"request_id":1,"model":"qwen3.8-27b-nvfp4"}},
            "result":{{"prompt_tokens":645,"completion_tokens":155,"model_thinking_tokens":0,"finish_reason":"stop_token","prefix_cache_hit_tokens":0,"prefix_reuse_path":"root","tool_call_count":0,"tool_call_parse":{{"structured_call_count":0,"fallback_reason":"none","marker_seen":false}}}},
            "timings_seconds":{{"ttft":0.4219088,"prefill":0.4204818,"decode":1.3753831,"prepare":0.0006098,"total":3.9243131,"vision":0.0}},
            "engine_timing":{{"queue_wait_seconds":0.0010093,"device_wait_exposed_seconds":3.1007284,"host_exposed_seconds":{{"engine_boundary":0.0023163,"engine_commit_output":0.0025529,"engine_maintenance":0.0000637,"program_post":0.0003812,"program_submit":0.8165297,"total":0.8218438}},"decode":{{"device_wait_exposed_seconds":1.3696447,"host_exposed_seconds":0.007073,"rounds":47}},"units":{{"control":0,"prefill":3}}}},
            "speculative":{{"backend":"mtp","drafted_tokens":141,"accepted_tokens":109,"accepted_per_position":[40,37,32],"fallback_steps":0,"rounds":47}}}}"#
        );
        let ParsedEvent::RequestDone(e) = parse_line(raw.as_bytes()).unwrap() else {
            panic!("expected RequestDone");
        };
        assert_eq!(e.request.request_id, Some(1));
        assert_eq!(e.result.prompt_tokens, Some(645));
        assert_eq!(e.result.completion_tokens, Some(155));
        assert_eq!(e.result.model_thinking_tokens, Some(0));
        assert_eq!(e.result.finish_reason.as_deref(), Some("stop_token"));
        assert_eq!(e.result.prefix_reuse_path.as_deref(), Some("root"));
        assert_eq!(
            e.result.tool_call_parse.fallback_reason.as_deref(),
            Some("none")
        );
        assert_eq!(e.timings_seconds.ttft, Some(0.4219088));
        assert_eq!(e.timings_seconds.total, Some(3.9243131));
        assert_eq!(e.engine_timing.queue_wait_seconds, Some(0.0010093));
        assert_eq!(e.engine_timing.device_wait_exposed_seconds, Some(3.1007284));
        assert_eq!(e.engine_timing.host_exposed_seconds.total, Some(0.8218438));
        assert_eq!(
            e.engine_timing.host_exposed_seconds.engine_boundary,
            Some(0.0023163)
        );
        assert_eq!(
            e.engine_timing.host_exposed_seconds.engine_commit_output,
            Some(0.0025529)
        );
        assert_eq!(
            e.engine_timing.host_exposed_seconds.engine_maintenance,
            Some(0.0000637)
        );
        assert_eq!(
            e.engine_timing.host_exposed_seconds.program_post,
            Some(0.0003812)
        );
        assert_eq!(
            e.engine_timing.host_exposed_seconds.program_submit,
            Some(0.8165297)
        );
        assert_eq!(e.engine_timing.decode.rounds, Some(47));
        assert_eq!(
            e.engine_timing.decode.device_wait_exposed_seconds,
            Some(1.3696447)
        );
        assert_eq!(e.engine_timing.decode.host_exposed_seconds, Some(0.007073));
        assert_eq!(e.engine_timing.units.prefill, Some(3));
        assert_eq!(e.speculative.backend.as_deref(), Some("mtp"));
        assert_eq!(e.speculative.drafted_tokens, Some(141));
        assert_eq!(e.speculative.accepted_tokens, Some(109));
        assert_eq!(e.speculative.accepted_per_position, vec![40, 37, 32]);
        assert_eq!(e.speculative.rounds, Some(47));
    }

    #[test]
    fn request_done_engine_timing_missing_fields_default_to_none() {
        let raw = format!(
            r#"{{{ENVELOPE},"event":"request_done",
            "request":{{"request_id":1}},
            "engine_timing":{{"queue_wait_seconds":0.001}}}}"#
        );
        let ParsedEvent::RequestDone(e) = parse_line(raw.as_bytes()).unwrap() else {
            panic!("expected RequestDone");
        };
        assert_eq!(e.engine_timing.device_wait_exposed_seconds, None);
        assert_eq!(e.engine_timing.host_exposed_seconds.total, None);
        assert_eq!(e.engine_timing.host_exposed_seconds.program_submit, None);
        assert_eq!(e.engine_timing.decode.device_wait_exposed_seconds, None);
        assert_eq!(e.engine_timing.decode.host_exposed_seconds, None);
        assert_eq!(e.engine_timing.decode.rounds, None);
    }

    #[test]
    fn throughput_round_trip() {
        let raw = format!(
            r#"{{{ENVELOPE},"event":"throughput",
            "throughput_tokens_per_second":{{"decode":53.71620426468579,"prefill":2461.725450667429}},
            "tokens":{{"committed_decode":268,"computed_prefill":12282}},
            "decode_batch":{{"rounds":52,"average_size":1.5}},
            "scheduler":{{"running":1,"prefilling":1,"waiting":0,"materializing":0,"decode_ready":0,"capture_pending":0,"terminal_pending":0}},
            "context_cache":{{"occupancy":{{"device_main_kv_pages":1183,"device_backend_kv_pages":1183,"host_kv_bytes":0,"host_state_slots":1,"device_state_slots":4}},"pressure":{{"searches":1,"spill_pages":0,"partial_tail_cow_pages":4}}}},
            "host_work":{{"work_class_seconds":{{"control_device_wait":0.0,"control_host":0.0000387,"decode_device_wait":1.5606804,"decode_host":0.0079375,"prefill_device_wait":2.1231027,"prefill_host":0.9269909}}}},
            "interval_seconds":4.9891835}}"#
        );
        let ParsedEvent::Throughput(e) = parse_line(raw.as_bytes()).unwrap() else {
            panic!("expected Throughput");
        };
        assert_eq!(
            e.throughput_tokens_per_second.decode,
            Some(53.71620426468579)
        );
        assert_eq!(
            e.throughput_tokens_per_second.prefill,
            Some(2461.725450667429)
        );
        assert_eq!(e.tokens.committed_decode, Some(268));
        assert_eq!(e.tokens.computed_prefill, Some(12282));
        assert_eq!(e.decode_batch.rounds, Some(52));
        assert_eq!(e.decode_batch.average_size, Some(1.5));
        assert_eq!(e.scheduler.running, Some(1));
        assert_eq!(e.scheduler.prefilling, Some(1));
        assert_eq!(e.scheduler.waiting, Some(0));
        assert_eq!(e.context_cache.occupancy.device_main_kv_pages, Some(1183));
        assert_eq!(e.context_cache.occupancy.host_kv_bytes, Some(0));
        assert_eq!(e.context_cache.occupancy.device_state_slots, Some(4));
        assert_eq!(e.context_cache.pressure.searches, Some(1));
        assert_eq!(e.context_cache.pressure.partial_tail_cow_pages, Some(4));
        assert_eq!(
            e.host_work.work_class_seconds.decode_device_wait,
            Some(1.5606804)
        );
        assert_eq!(e.interval_seconds, Some(4.9891835));
    }

    #[test]
    fn request_error_round_trip() {
        let raw = format!(
            r#"{{{ENVELOPE},"event":"request_error",
            "request":{{"request_id":66,"model":"qwen3.8-27b-nvfp4","tool_count":9}},
            "error":{{"message":"inference request expired while waiting for admission"}}}}"#
        );
        let ParsedEvent::RequestError(e) = parse_line(raw.as_bytes()).unwrap() else {
            panic!("expected RequestError");
        };
        assert_eq!(e.request.request_id, Some(66));
        assert_eq!(e.request.tool_count, Some(9));
        assert_eq!(
            e.error.message.as_deref(),
            Some("inference request expired while waiting for admission")
        );
    }
}
