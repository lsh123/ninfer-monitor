// NInferMonitor
//
// This is free software; see the LICENSE.md file in the source distribution for precise wording.
//
// Copyright (C) 2026 Aleksey Sanin aleksey@aleksey.com. All Rights Reserved.

use crate::metrics::{ThroughputSample, WINDOW_MS, mean};
use crate::store::Store;

/// Shown by KPI cards that have no data yet (FR-3.3).
pub const NO_DATA: &str = "—";

/// Direction of change vs. the previous 60 s window (FR-3.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trend {
    Up,
    Down,
    Flat,
}

impl Trend {
    pub fn as_int(self) -> i32 {
        match self {
            Trend::Up => 1,
            Trend::Down => -1,
            Trend::Flat => 0,
        }
    }
}

/// Display state of one KPI value (FR-3). The unit is carried by the widget
/// title (e.g. "Throughput (tok/s)"), not by the value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Kpi {
    pub value: String,
    pub trend: Trend,
}

impl Kpi {
    fn missing() -> Self {
        Self {
            value: NO_DATA.to_owned(),
            trend: Trend::Flat,
        }
    }
}

/// The KPI values from FR-3.1, formatted for display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KpiSnapshot {
    pub decode: Kpi,
    pub prefill: Kpi,
    pub active: Kpi,
    pub waiting: Kpi,
    pub ttft: Kpi,
    pub decode_latency: Kpi,
    pub cache_hit: Kpi,
    pub spec_accept: Kpi,
}

/// Compute the display state of all KPI values from the store (FR-3).
///
/// Decode/prefill show the latest `throughput` sample; TTFT, decode latency,
/// cache hit rate, and speculative acceptance are windowed over the last 60 s;
/// active/waiting show the latest scheduler `running`/`waiting` counts.
/// Trends compare against the previous 60 s window (FR-3.2).
pub fn snapshot(store: &Store) -> KpiSnapshot {
    let w = store.window();
    KpiSnapshot {
        decode: throughput_kpi(store, |s| s.decode_tps),
        prefill: throughput_kpi(store, |s| s.prefill_tps),
        active: active_kpi(store),
        waiting: waiting_kpi(store),
        ttft: windowed_kpi(w.avg_ttft(), w.avg_ttft_previous(), fmt_ms),
        decode_latency: windowed_kpi(
            w.avg_decode_latency_per_token(),
            w.avg_decode_latency_per_token_previous(),
            |v| fmt_1dp(v * 1000.0),
        ),
        cache_hit: windowed_kpi(
            w.prefix_cache_hit_rate(),
            w.prefix_cache_hit_rate_previous(),
            fmt_pct,
        ),
        spec_accept: windowed_kpi(
            w.speculative_acceptance(),
            w.speculative_acceptance_previous(),
            fmt_pct,
        ),
    }
}

#[macro_export]
macro_rules! apply_kpis {
    ($window:expr, $snapshot:expr) => {{
        let k = $snapshot;
        $window.set_kpi_decode_value(k.decode.value.as_str().into());
        $window.set_kpi_decode_trend(k.decode.trend.as_int());
        $window.set_kpi_prefill_value(k.prefill.value.as_str().into());
        $window.set_kpi_prefill_trend(k.prefill.trend.as_int());
        $window.set_kpi_active_value(k.active.value.as_str().into());
        $window.set_kpi_active_trend(k.active.trend.as_int());
        $window.set_kpi_waiting_value(k.waiting.value.as_str().into());
        $window.set_kpi_waiting_trend(k.waiting.trend.as_int());
        $window.set_kpi_ttft_value(k.ttft.value.as_str().into());
        $window.set_kpi_ttft_trend(k.ttft.trend.as_int());
        $window.set_kpi_decode_lat_value(k.decode_latency.value.as_str().into());
        $window.set_kpi_decode_lat_trend(k.decode_latency.trend.as_int());
        $window.set_kpi_cache_value(k.cache_hit.value.as_str().into());
        $window.set_kpi_cache_trend(k.cache_hit.trend.as_int());
        $window.set_kpi_spec_value(k.spec_accept.value.as_str().into());
        $window.set_kpi_spec_trend(k.spec_accept.trend.as_int());
    }};
}

/// Latest `throughput` sample with a trend vs. the mean of the previous
/// 60 s window (FR-3.1/FR-3.2).
fn throughput_kpi(store: &Store, field: impl Fn(&ThroughputSample) -> Option<f64>) -> Kpi {
    let Some(latest) = store.latest_throughput() else {
        return Kpi::missing();
    };
    let Some(value) = field(latest) else {
        return Kpi::missing();
    };
    let previous = previous_window_mean(store, latest.timestamp_ms, field);
    Kpi {
        value: fmt_1dp(value),
        trend: compare(Some(value), previous),
    }
}

/// Mean of `field` over throughput samples in the previous 60 s window
/// `[anchor - 2*WINDOW_MS, anchor - WINDOW_MS)`.
fn previous_window_mean(
    store: &Store,
    anchor: u64,
    field: impl Fn(&ThroughputSample) -> Option<f64>,
) -> Option<f64> {
    // TODO(FR-3.2): anchors the previous window to the latest throughput
    // sample, while the windowed KPIs anchor to the newest event timestamp
    // (`Window::anchor`); unify the reference frames if the drift matters.
    let lo = anchor.saturating_sub(2 * WINDOW_MS);
    let hi = anchor.saturating_sub(WINDOW_MS);
    let values: Vec<f64> = store
        .throughput()
        .iter()
        .filter(|s| s.timestamp_ms >= lo && s.timestamp_ms < hi)
        .filter_map(field)
        .collect();
    mean(&values)
}

/// Latest scheduler `running` count (FR-3.1). Active requests always show a
/// flat trend (FR-3.2).
fn active_kpi(store: &Store) -> Kpi {
    let kpi = scheduler_kpi(store, |t| t.scheduler_running);
    Kpi {
        trend: Trend::Flat,
        ..kpi
    }
}

/// Latest scheduler `waiting` count (FR-3.1).
fn waiting_kpi(store: &Store) -> Kpi {
    scheduler_kpi(store, |t| t.scheduler_waiting)
}

/// Latest value of a scheduler counter from the newest `throughput` sample,
/// with a trend vs. the mean of the previous 60 s window (FR-3.1/FR-3.2).
fn scheduler_kpi(store: &Store, field: impl Fn(&ThroughputSample) -> Option<u64>) -> Kpi {
    let Some(latest) = store.latest_throughput() else {
        return Kpi::missing();
    };
    let Some(value) = field(latest) else {
        return Kpi::missing();
    };
    let previous = previous_window_mean(store, latest.timestamp_ms, |s| field(s).map(|v| v as f64));
    Kpi {
        value: value.to_string(),
        trend: compare(Some(value as f64), previous),
    }
}

fn windowed_kpi(current: Option<f64>, previous: Option<f64>, fmt: impl Fn(f64) -> String) -> Kpi {
    match current {
        Some(value) => Kpi {
            value: fmt(value),
            trend: compare(current, previous),
        },
        None => Kpi {
            value: NO_DATA.to_owned(),
            trend: Trend::Flat,
        },
    }
}

fn compare(current: Option<f64>, previous: Option<f64>) -> Trend {
    match (current, previous) {
        (Some(c), Some(p)) if c > p => Trend::Up,
        (Some(c), Some(p)) if c < p => Trend::Down,
        _ => Trend::Flat,
    }
}

fn fmt_ms(seconds: f64) -> String {
    format!("{}", (seconds * 1000.0).round() as u64)
}

fn fmt_1dp(value: f64) -> String {
    format!("{value:.1}")
}

fn fmt_pct(rate: f64) -> String {
    format!("{:.1}", rate * 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{ParsedEvent, parse_line};

    fn throughput_line(ts: u64, decode: f64, prefill: f64) -> ParsedEvent {
        let raw = format!(
            r#"{{"artifact_type":"ninfer_serve_request_log","schema_version":20,"server_instance_id":"s","timestamp_unix_ms":{ts},"event":"throughput","throughput_tokens_per_second":{{"decode":{decode},"prefill":{prefill}}}}}"#
        );
        parse_line(raw.as_bytes()).unwrap()
    }

    fn throughput_line_with_scheduler(
        ts: u64,
        running: u64,
        prefilling: u64,
        waiting: u64,
    ) -> ParsedEvent {
        let raw = format!(
            r#"{{"artifact_type":"ninfer_serve_request_log","schema_version":20,"server_instance_id":"s","timestamp_unix_ms":{ts},"event":"throughput","throughput_tokens_per_second":{{"decode":1.0,"prefill":2.0}},"scheduler":{{"running":{running},"prefilling":{prefilling},"waiting":{waiting}}}}}"#
        );
        parse_line(raw.as_bytes()).unwrap()
    }

    fn request_start_line(ts: u64, id: u64) -> ParsedEvent {
        let raw = format!(
            r#"{{"artifact_type":"ninfer_serve_request_log","schema_version":20,"server_instance_id":"s","timestamp_unix_ms":{ts},"event":"request_start","request":{{"request_id":{id}}}}}"#
        );
        parse_line(raw.as_bytes()).unwrap()
    }

    fn request_done_line(ts: u64, id: u64, ttft: f64) -> ParsedEvent {
        let raw = format!(
            r#"{{"artifact_type":"ninfer_serve_request_log","schema_version":20,"server_instance_id":"s","timestamp_unix_ms":{ts},"event":"request_done","request":{{"request_id":{id}}},"result":{{"prompt_tokens":100,"completion_tokens":50,"prefix_cache_hit_tokens":25}},"timings_seconds":{{"ttft":{ttft},"decode":1.0,"total":1.5}},"speculative":{{"drafted_tokens":10,"accepted_tokens":5}}}}"#
        );
        parse_line(raw.as_bytes()).unwrap()
    }

    #[test]
    fn empty_store_shows_no_data() {
        let store = Store::new();
        let k = snapshot(&store);
        for kpi in [
            &k.decode,
            &k.prefill,
            &k.active,
            &k.waiting,
            &k.ttft,
            &k.decode_latency,
            &k.cache_hit,
            &k.spec_accept,
        ] {
            assert_eq!(kpi.value, NO_DATA);
            assert_eq!(kpi.trend, Trend::Flat);
        }
    }

    #[test]
    fn throughput_value_and_trend_up() {
        let mut store = Store::new();
        store.apply(&throughput_line(0, 100.0, 10.0));
        store.apply(&throughput_line(30_000, 110.0, 20.0));
        store.apply(&throughput_line(90_000, 120.0, 30.0));
        let k = snapshot(&store);
        assert_eq!(k.decode.value, "120.0");
        // Previous window [0, 30_000) holds only the ts=0 sample (100).
        assert_eq!(k.decode.trend, Trend::Up);
        assert_eq!(k.prefill.value, "30.0");
        assert_eq!(k.prefill.trend, Trend::Up);
    }

    #[test]
    fn throughput_trend_down() {
        let mut store = Store::new();
        store.apply(&throughput_line(0, 200.0, 100.0));
        store.apply(&throughput_line(90_000, 100.0, 50.0));
        let k = snapshot(&store);
        assert_eq!(k.decode.value, "100.0");
        assert_eq!(k.decode.trend, Trend::Down);
        assert_eq!(k.prefill.trend, Trend::Down);
    }

    #[test]
    fn throughput_trend_flat_without_previous_data() {
        let mut store = Store::new();
        store.apply(&throughput_line(90_000, 120.0, 30.0));
        let k = snapshot(&store);
        assert_eq!(k.decode.trend, Trend::Flat);
        assert_eq!(k.prefill.trend, Trend::Flat);
    }

    #[test]
    fn throughput_missing_fields_show_no_data() {
        let mut store = Store::new();
        let raw = r#"{"artifact_type":"ninfer_serve_request_log","schema_version":20,"server_instance_id":"s","timestamp_unix_ms":1000,"event":"throughput"}"#;
        store.apply(&parse_line(raw.as_bytes()).unwrap());
        let k = snapshot(&store);
        assert_eq!(k.decode.value, NO_DATA);
        assert_eq!(k.prefill.value, NO_DATA);
        assert_eq!(k.decode.trend, Trend::Flat);
    }

    #[test]
    fn windowed_kpi_values_and_trend() {
        let mut store = Store::new();
        // Anchor 100_000: current window [40_000, 100_000], previous
        // window [0, 40_000).
        store.apply(&request_done_line(20_000, 1, 1.0));
        store.apply(&request_done_line(50_000, 2, 0.5));
        store.apply(&throughput_line(100_000, 1.0, 2.0));
        let k = snapshot(&store);
        assert_eq!(k.ttft.value, "500");
        assert_eq!(k.ttft.trend, Trend::Down);
        assert_eq!(k.decode_latency.value, "20.0");
        assert_eq!(k.decode_latency.trend, Trend::Flat);
        assert_eq!(k.cache_hit.value, "25.0");
        assert_eq!(k.cache_hit.trend, Trend::Flat);
        assert_eq!(k.spec_accept.value, "50.0");
        assert_eq!(k.spec_accept.trend, Trend::Flat);
    }

    #[test]
    fn windowed_kpi_trend_up() {
        let mut store = Store::new();
        store.apply(&request_done_line(20_000, 1, 0.5));
        store.apply(&request_done_line(50_000, 2, 1.0));
        store.apply(&throughput_line(100_000, 1.0, 2.0));
        let k = snapshot(&store);
        assert_eq!(k.ttft.value, "1000");
        assert_eq!(k.ttft.trend, Trend::Up);
    }

    #[test]
    fn windowed_kpi_no_data_in_window() {
        let mut store = Store::new();
        // The only completed request fell out of the current window.
        store.apply(&request_done_line(0, 1, 1.0));
        store.apply(&throughput_line(100_000, 1.0, 2.0));
        let k = snapshot(&store);
        assert_eq!(k.ttft.value, NO_DATA);
        assert_eq!(k.ttft.trend, Trend::Flat);
        assert_eq!(k.cache_hit.value, NO_DATA);
        assert_eq!(k.spec_accept.value, NO_DATA);
    }

    #[test]
    fn active_and_waiting_kpi_show_scheduler() {
        let mut store = Store::new();
        store.apply(&request_start_line(1_000, 1));
        store.apply(&request_start_line(2_000, 2));
        store.apply(&throughput_line_with_scheduler(3_000, 1, 0, 2));
        let k = snapshot(&store);
        assert_eq!(k.active.value, "1");
        assert_eq!(k.active.trend, Trend::Flat);
        assert_eq!(k.waiting.value, "2");
        assert_eq!(k.waiting.trend, Trend::Flat);
    }

    #[test]
    fn scheduler_trend_vs_previous_window() {
        let mut store = Store::new();
        store.apply(&throughput_line_with_scheduler(0, 2, 0, 0));
        store.apply(&throughput_line_with_scheduler(90_000, 1, 0, 4));
        let k = snapshot(&store);
        // Previous window [0, 30_000) holds only the ts=0 sample
        // (running 2, waiting 0). Active is always flat (FR-3.2).
        assert_eq!(k.active.value, "1");
        assert_eq!(k.active.trend, Trend::Flat);
        assert_eq!(k.waiting.value, "4");
        assert_eq!(k.waiting.trend, Trend::Up);
    }

    #[test]
    fn active_kpi_without_scheduler_data() {
        let mut store = Store::new();
        store.apply(&request_start_line(1_000, 1));
        store.apply(&request_start_line(2_000, 2));
        let k = snapshot(&store);
        assert_eq!(k.active.value, NO_DATA);
        assert_eq!(k.waiting.value, NO_DATA);
    }

    #[test]
    fn formatting_matches_prd_reference_values() {
        assert_eq!(fmt_ms(0.4219088), "422");
        assert_eq!(fmt_1dp(123.89240590071061), "123.9");
        assert_eq!(fmt_1dp(0.0), "0.0");
        assert_eq!(fmt_1dp(0.007278349731205014 * 1000.0), "7.3");
        assert_eq!(fmt_pct(0.9969298439400317), "99.7");
        assert_eq!(fmt_pct(0.8274085522926327), "82.7");
    }
}
