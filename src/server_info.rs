// NInferMonitor
//
// This is free software; see the LICENSE.md file in the source distribution for precise wording.
//
// Copyright (C) 2026 Aleksey Sanin aleksey@aleksey.com. All Rights Reserved.

use crate::parser::ServerStartEvent;
use crate::store::Store;

/// Shown for missing values (FR-3.3 convention).
const NO_DATA: &str = "—";

/// Display state of the server info panel (FR-6.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerInfoSnapshot {
    pub summary: String,
    pub model: String,
    pub engine: String,
    pub server: String,
}

/// Compute the display state of the server info panel from the latest
/// `server_start` (FR-6.1). The panel updates automatically on a server
/// restart because the store keeps only the newest `server_start` (FR-6.2).
pub fn snapshot(store: &Store) -> ServerInfoSnapshot {
    let Some(s) = store.server() else {
        return ServerInfoSnapshot {
            summary: "No server info yet".to_owned(),
            model: NO_DATA.to_owned(),
            engine: NO_DATA.to_owned(),
            server: NO_DATA.to_owned(),
        };
    };
    ServerInfoSnapshot {
        summary: summary(s),
        model: model_line(s),
        engine: engine_line(s),
        server: server_line(s),
    }
}

/// True when the panel must be (re)applied: nothing was applied yet or the
/// current snapshot differs from the last-applied one. Comparing snapshots
/// instead of a `server_start` counter keeps the panel in sync when the
/// store is reset (server restart) or the log file is switched (startup
/// Start, Settings Save).
pub fn needs_update(last: &Option<ServerInfoSnapshot>, current: &ServerInfoSnapshot) -> bool {
    last.as_ref() != Some(current)
}

#[macro_export]
macro_rules! apply_server_info {
    ($window:expr, $info:expr) => {{
        let s = $info;
        $window.set_server_info_summary(s.summary.as_str().into());
        $window.set_server_info_model(s.model.as_str().into());
        $window.set_server_info_engine(s.engine.as_str().into());
        $window.set_server_info_server(s.server.as_str().into());
    }};
}

fn summary(s: &ServerStartEvent) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(gpu) = &s.environment.gpu_name {
        parts.push(gpu.clone());
    }
    if let Some(model) = model_id(s) {
        parts.push(model);
    }
    let kv = kv_label(s);
    if !kv.is_empty() {
        parts.push(kv);
    }
    if let Some(free) = s.memory.available_after_weights_bytes {
        parts.push(format!("{} free", fmt_bytes(free)));
    }
    join_or_no_data(parts)
}

/// The schema 21 artifact label: the artifact name (or the architecture
/// when the name is empty) with the weight formats in parentheses.
fn artifact_label(s: &ServerStartEvent) -> Option<String> {
    let label = s
        .artifact
        .name
        .clone()
        .filter(|name| !name.is_empty())
        .or_else(|| s.artifact.architecture.clone())?;
    let formats = s.artifact.formats.join(", ");
    if formats.is_empty() {
        Some(label)
    } else {
        Some(format!("{label} ({formats})"))
    }
}

/// The display model id: `server.public_model_id`, then
/// `engine.public_model_id`, then `engine.model_id`, then the schema 21
/// artifact label, then `artifact.target` + `artifact.weights_id`.
fn model_id(s: &ServerStartEvent) -> Option<String> {
    s.server
        .public_model_id
        .clone()
        .or_else(|| s.engine.public_model_id.clone())
        .or_else(|| s.engine.model_id.clone())
        .or_else(|| artifact_label(s))
        .or_else(|| match (&s.artifact.target, &s.artifact.weights_id) {
            (Some(target), Some(weights_id)) => Some(format!("{target}-{weights_id}")),
            _ => None,
        })
}

fn kv_label(s: &ServerStartEvent) -> String {
    match (&s.engine.kv_cache, s.engine.kv_capacity) {
        (Some(cache), Some(capacity)) => format!("KV {cache} {}", fmt_int(capacity)),
        (None, Some(capacity)) => format!("KV {}", fmt_int(capacity)),
        (Some(cache), None) => format!("KV {cache}"),
        (None, None) => String::new(),
    }
}

fn model_line(s: &ServerStartEvent) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(label) = artifact_label(s) {
        parts.push(label);
    } else if let Some(target) = &s.artifact.target {
        parts.push(match &s.artifact.weights_id {
            Some(weights_id) => format!("{target} ({weights_id})"),
            None => target.clone(),
        });
    }
    if let Some(size) = s.artifact.size_bytes {
        parts.push(fmt_bytes(size));
    }
    if let Some(load) = s.artifact.load_seconds {
        parts.push(format!("load {} s", fmt_seconds(load)));
    }
    if let Some(upload) = s.artifact.upload_seconds {
        parts.push(format!("upload {} s", fmt_seconds(upload)));
    }
    join_or_no_data(parts)
}

fn engine_line(s: &ServerStartEvent) -> String {
    let e = &s.engine;
    let mut parts: Vec<String> = Vec::new();
    if let Some(id) = e.public_model_id.clone().or_else(|| e.model_id.clone()) {
        parts.push(id);
    }
    if let Some(cache) = &e.kv_cache {
        parts.push(format!("KV {cache}"));
    }
    if let Some(capacity) = e.kv_capacity {
        parts.push(format!("capacity {}", fmt_int(capacity)));
    }
    if let Some(max_context) = e.max_context {
        parts.push(format!("max context {}", fmt_int(max_context)));
    }
    if let Some(concurrency) = e.max_concurrency {
        parts.push(format!("concurrency {concurrency}"));
    }
    if let Some(chunk) = e.prefill_chunk {
        parts.push(format!("prefill chunk {}", fmt_int(chunk)));
    }
    if let Some(spec) = &e.speculative_backend {
        parts.push(format!("spec {spec}"));
    }
    if let Some(cuda_graph) = e.cuda_graph {
        parts.push(format!("cuda graph {}", bool_str(cuda_graph)));
    }
    if let Some(prefix_reuse) = e.prefix_reuse {
        parts.push(format!("prefix reuse {}", bool_str(prefix_reuse)));
    }
    join_or_no_data(parts)
}

fn server_line(s: &ServerStartEvent) -> String {
    let mut parts: Vec<String> = Vec::new();
    let host_port = match (&s.server.host, s.server.port) {
        (Some(host), Some(port)) => Some(format!("{host}:{port}")),
        (Some(host), None) => Some(host.clone()),
        (None, Some(port)) => Some(port.to_string()),
        (None, None) => None,
    };
    if let Some(host_port) = host_port {
        parts.push(host_port);
    }
    if let Some(id) = &s.server_instance_id {
        parts.push(id.clone());
    }
    if let Some(log) = &s.server.request_log_jsonl {
        parts.push(format!("log {log}"));
    }
    join_or_no_data(parts)
}

fn join_or_no_data(parts: Vec<String>) -> String {
    if parts.is_empty() {
        NO_DATA.to_owned()
    } else {
        parts.join(" · ")
    }
}

/// `240000` -> `240 000` (space thousands separator, PRD M10 reference values).
fn fmt_int(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(' ');
        }
        out.push(c);
    }
    out
}

/// Bytes as GiB (2^30) with one decimal place.
fn fmt_bytes(value: u64) -> String {
    format!("{} GiB", fmt_gib(value))
}

fn fmt_gib(value: u64) -> String {
    format!("{:.1}", value as f64 / 1073741824.0)
}

fn fmt_seconds(value: f64) -> String {
    format!("{value:.1}")
}

fn bool_str(value: bool) -> &'static str {
    if value { "on" } else { "off" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{ParsedEvent, parse_line};

    fn sample_store() -> Store {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/server.requests.jsonl");
        Store::load_jsonl(path).expect("read sample log")
    }

    fn server_start_line(ts: u64, body: &str) -> ParsedEvent {
        let raw = if body.is_empty() {
            format!(
                r#"{{"artifact_type":"ninfer_serve_request_log","schema_version":20,"server_instance_id":"s","timestamp_unix_ms":{ts},"event":"server_start"}}"#
            )
        } else {
            format!(
                r#"{{"artifact_type":"ninfer_serve_request_log","schema_version":20,"server_instance_id":"s","timestamp_unix_ms":{ts},"event":"server_start",{body}}}"#
            )
        };
        parse_line(raw.as_bytes()).unwrap()
    }

    fn server_start_line_v21(ts: u64, body: &str) -> ParsedEvent {
        let raw = format!(
            r#"{{"artifact_type":"ninfer_serve_request_log","schema_version":21,"server_instance_id":"s","timestamp_unix_ms":{ts},"event":"server_start",{body}}}"#
        );
        parse_line(raw.as_bytes()).unwrap()
    }

    #[test]
    fn empty_store_snapshot_shows_no_data() {
        let s = snapshot(&Store::new());
        assert_eq!(s.summary, "No server info yet");
        assert_eq!(s.model, NO_DATA);
        assert_eq!(s.engine, NO_DATA);
        assert_eq!(s.server, NO_DATA);
    }

    #[test]
    fn sample_log_snapshot_matches_prd_reference_values() {
        let s = snapshot(&sample_store());
        assert_eq!(
            s.summary,
            "NVIDIA GeForce RTX 5090 · qwen3.8-27b-nvfp4 · KV fp8-e4m3-row256 240 000 · 10.5 GiB free"
        );
        assert_eq!(
            s.model,
            "qwen3_8_27b (nvfp4) · 22.1 GiB · load 9.7 s · upload 6.8 s"
        );
        assert_eq!(
            s.engine,
            "KV fp8-e4m3-row256 · capacity 240 000 · max context 240 000 · concurrency 2 · prefill chunk 2 048 · spec mtp · cuda graph on · prefix reuse on"
        );
        assert_eq!(
            s.server,
            "0.0.0.0:8080 · serve-27872-1789281769048204 · log d:/NInfer/logs/server.requests.jsonl"
        );
    }

    #[test]
    fn snapshot_updates_on_restart() {
        let mut store = sample_store();
        let ts = store.max_timestamp_ms().expect("last timestamp") + 1_000;
        let raw = format!(
            r#"{{"artifact_type":"ninfer_serve_request_log","schema_version":20,"server_instance_id":"serve-2","timestamp_unix_ms":{ts},"event":"server_start","engine":{{"kv_cache":"bf16","kv_capacity":120000,"max_context":120000,"max_concurrency":4}},"environment":{{"gpu_name":"NVIDIA GeForce RTX 4090"}},"memory":{{"available_after_weights_bytes":8589934592}},"server":{{"public_model_id":"qwen3.8-27b-fp8"}}}}"#
        );
        store.apply(&parse_line(raw.as_bytes()).unwrap());
        let s = snapshot(&store);
        assert_eq!(
            s.summary,
            "NVIDIA GeForce RTX 4090 · qwen3.8-27b-fp8 · KV bf16 120 000 · 8.0 GiB free"
        );
        assert_eq!(
            s.engine,
            "KV bf16 · capacity 120 000 · max context 120 000 · concurrency 4"
        );
        assert_eq!(s.server, "serve-2");
        assert_eq!(s.model, NO_DATA);
    }

    #[test]
    fn model_id_prefers_server_public_model_id() {
        let mut store = Store::new();
        store.apply(&server_start_line(
            1_000,
            r#""engine":{"public_model_id":"b","model_id":"c"},"artifact":{"target":"d","weights_id":"e"},"server":{"public_model_id":"a"}"#,
        ));
        let s = snapshot(&store);
        assert_eq!(s.summary, "a", "server.public_model_id must win");
    }

    #[test]
    fn model_id_prefers_engine_public_model_id() {
        let mut store = Store::new();
        store.apply(&server_start_line(
            1_000,
            r#""engine":{"public_model_id":"b","model_id":"c"},"artifact":{"target":"d","weights_id":"e"}"#,
        ));
        let s = snapshot(&store);
        assert_eq!(s.summary, "b", "engine.public_model_id must win");
    }

    #[test]
    fn model_id_falls_back_to_engine_model_id() {
        let mut store = Store::new();
        store.apply(&server_start_line(
            1_000,
            r#""engine":{"model_id":"c"},"artifact":{"target":"d","weights_id":"e"}"#,
        ));
        let s = snapshot(&store);
        assert_eq!(
            s.summary, "c",
            "engine.model_id must win over artifact.target"
        );
    }

    #[test]
    fn model_id_falls_back_to_target_and_weights_id() {
        let mut store = Store::new();
        store.apply(&server_start_line(
            1_000,
            r#""artifact":{"target":"d","weights_id":"e"}"#,
        ));
        let s = snapshot(&store);
        assert!(s.summary.contains("d-e"), "summary: {}", s.summary);
    }

    #[test]
    fn minimal_server_start_shows_no_data_lines() {
        let mut store = Store::new();
        store.apply(&server_start_line(1_000, ""));
        let s = snapshot(&store);
        assert_eq!(s.summary, NO_DATA);
        assert_eq!(s.model, NO_DATA);
        assert_eq!(s.engine, NO_DATA);
        assert_eq!(s.server, "s");
    }

    #[test]
    fn fmt_int_groups_thousands_with_spaces() {
        assert_eq!(fmt_int(2), "2");
        assert_eq!(fmt_int(2048), "2 048");
        assert_eq!(fmt_int(240000), "240 000");
        assert_eq!(fmt_int(1000000), "1 000 000");
    }

    #[test]
    fn fmt_gib_one_decimal() {
        assert_eq!(fmt_gib(23719496192), "22.1");
        assert_eq!(fmt_gib(9080226048), "8.5");
        assert_eq!(fmt_gib(319963136), "0.3");
        assert_eq!(fmt_gib(0), "0.0");
    }

    #[test]
    fn fmt_bytes_uses_gib_unit() {
        assert_eq!(fmt_bytes(1073741824), "1.0 GiB");
        assert_eq!(fmt_bytes(11310989312), "10.5 GiB");
        assert_eq!(fmt_bytes(0), "0.0 GiB");
    }

    #[test]
    fn needs_update_on_first_snapshot() {
        let s = snapshot(&Store::new());
        assert!(needs_update(&None, &s));
    }

    #[test]
    fn needs_update_false_when_unchanged() {
        let s = snapshot(&Store::new());
        assert!(!needs_update(&Some(s.clone()), &s));
    }

    #[test]
    fn needs_update_true_when_changed() {
        let mut store = Store::new();
        let before = snapshot(&store);
        store.apply(&server_start_line(
            1_000,
            r#""engine":{"kv_capacity":240000}"#,
        ));
        let after = snapshot(&store);
        assert!(needs_update(&Some(before.clone()), &after));
    }

    #[test]
    fn needs_update_true_after_store_reset() {
        let store = sample_store();
        let live = snapshot(&store);
        let cleared = snapshot(&Store::new());
        assert!(
            needs_update(&Some(live.clone()), &cleared),
            "a cleared store must re-apply the empty panel state"
        );
    }

    #[test]
    fn server_line_host_without_port() {
        let mut store = Store::new();
        store.apply(&server_start_line(1_000, r#""server":{"host":"10.0.0.5"}"#));
        assert_eq!(snapshot(&store).server, "10.0.0.5 · s");
    }

    #[test]
    fn server_line_port_without_host() {
        let mut store = Store::new();
        store.apply(&server_start_line(1_000, r#""server":{"port":9090}"#));
        assert_eq!(snapshot(&store).server, "9090 · s");
    }

    #[test]
    fn engine_line_bools_off() {
        let mut store = Store::new();
        store.apply(&server_start_line(
            1_000,
            r#""engine":{"cuda_graph":false,"prefix_reuse":false}"#,
        ));
        assert_eq!(snapshot(&store).engine, "cuda graph off · prefix reuse off");
    }

    #[test]
    fn model_line_target_without_weights_id() {
        let mut store = Store::new();
        store.apply(&server_start_line(1_000, r#""artifact":{"target":"d"}"#));
        assert_eq!(snapshot(&store).model, "d");
    }

    #[test]
    fn model_id_prefers_public_model_id_over_artifact_name() {
        let mut store = Store::new();
        store.apply(&server_start_line_v21(
            1_000,
            r#""artifact":{"name":"qwen3_8_27b_nvfp4","formats":["nvfp4"]},"server":{"public_model_id":"qwen3.8-27b-nvfp4"}"#,
        ));
        let s = snapshot(&store);
        assert_eq!(s.summary, "qwen3.8-27b-nvfp4");
    }

    #[test]
    fn model_id_falls_back_to_artifact_name_and_formats() {
        let mut store = Store::new();
        store.apply(&server_start_line_v21(
            1_000,
            r#""artifact":{"name":"qwen3_8_27b_nvfp4","architecture":"qwen3_8_27b","formats":["nvfp4","bf16"],"size_bytes":1073741824}"#,
        ));
        let s = snapshot(&store);
        assert_eq!(
            s.summary, "qwen3_8_27b_nvfp4 (nvfp4, bf16)",
            "the artifact name with formats must win"
        );
        assert_eq!(s.model, "qwen3_8_27b_nvfp4 (nvfp4, bf16) · 1.0 GiB");
    }

    #[test]
    fn model_id_falls_back_to_architecture_when_name_empty() {
        let mut store = Store::new();
        store.apply(&server_start_line_v21(
            1_000,
            r#""artifact":{"name":"","architecture":"qwen3_8_27b","formats":["nvfp4"],"size_bytes":23719496192,"load_seconds":9.6972125,"upload_seconds":6.8405596}"#,
        ));
        let s = snapshot(&store);
        assert_eq!(s.summary, "qwen3_8_27b (nvfp4)");
        assert_eq!(
            s.model,
            "qwen3_8_27b (nvfp4) · 22.1 GiB · load 9.7 s · upload 6.8 s"
        );
    }

    #[test]
    fn summary_kv_cache_only() {
        let mut store = Store::new();
        store.apply(&server_start_line(1_000, r#""engine":{"kv_cache":"bf16"}"#));
        assert_eq!(snapshot(&store).summary, "KV bf16");
    }

    #[test]
    fn summary_kv_capacity_only() {
        let mut store = Store::new();
        store.apply(&server_start_line(
            1_000,
            r#""engine":{"kv_capacity":120000}"#,
        ));
        assert_eq!(snapshot(&store).summary, "KV 120 000");
    }
}
