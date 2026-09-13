// NInferMonitor
//
// This is free software; see the LICENSE.md file in the source distribution for precise wording.
//
// Copyright (C) 2026 Aleksey Sanin aleksey@aleksey.com. All Rights Reserved.

use std::hash::{Hash, Hasher};

use chrono::{DateTime, Local, Utc};
use plotters::element::PathElement;
use plotters::prelude::*;
use plotters::series::LineSeries;
use plotters::style::text_anchor::{HPos, Pos, VPos};
use plotters::style::{RGBColor, ShapeStyle};

use crate::metrics::{WINDOW_MS, WindowRecord, prefix_cache_hit_rate, speculative_acceptance};
use crate::store::Store;

/// Fallback render size of a chart image (FR-4.1), used when the panel size
/// is not known yet. The UI normally renders at the actual panel size so the
/// plot is not distorted.
pub const CHART_WIDTH: u32 = 640;
pub const CHART_HEIGHT: u32 = 240;

/// Width reserved for each y-axis tick label column (FR-4.3).
const Y_LABEL_AREA: u32 = 44;
/// Outer margin around the plot area.
const MARGIN: u32 = 8;

const BG: RGBColor = RGBColor(0x1e, 0x1e, 0x1e);
const TICK: RGBColor = RGBColor(0x9e, 0x9e, 0x9e);
const GRID: RGBColor = RGBColor(0x33, 0x33, 0x33);
const RESTART: RGBColor = RGBColor(0xab, 0x47, 0xbc);

/// Line color of the left-axis series (yellow), unified across all charts.
const LEFT_LINE: [u8; 3] = [0xff, 0xb7, 0x4d];
/// Line color of the right-axis series (blue), unified across all charts.
const RIGHT_LINE: [u8; 3] = [0x4f, 0xc3, 0xf7];

fn rgb(c: [u8; 3]) -> RGBColor {
    RGBColor(c[0], c[1], c[2])
}

/// The four required charts (FR-4.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChartKind {
    Throughput,
    Latency,
    Cache,
    Scheduler,
}

/// One time series: `(timestamp_ms, value)` points, oldest first.
#[derive(Debug, Clone)]
pub struct Series {
    pub name: &'static str,
    pub unit: &'static str,
    pub points: Vec<(f64, f64)>,
}

/// Data prepared for one chart within a time window (FR-4.2/FR-4.4).
#[derive(Debug, Clone)]
pub struct ChartData {
    pub kind: ChartKind,
    pub domain: (f64, f64),
    pub series: Vec<Series>,
    pub restarts: Vec<f64>,
}

/// A time series of `(timestamp_ms, value)` points, oldest first.
type PointSeries = Vec<(f64, f64)>;

/// The four chart images.
pub struct ChartImages {
    pub throughput: slint::Image,
    pub latency: slint::Image,
    pub cache: slint::Image,
    pub scheduler: slint::Image,
}

/// Local time with millisecond precision (NFR-5).
pub fn format_time_ms(ts_ms: f64) -> String {
    match DateTime::<Utc>::from_timestamp_millis(ts_ms as i64) {
        Some(dt) => dt.with_timezone(&Local).format("%H:%M:%S%.3f").to_string(),
        None => String::new(),
    }
}

/// Windowed cache-hit and speculative-acceptance rates (as percentages)
/// anchored at each completed request's timestamp.
fn cache_spec_series(records: &[&WindowRecord], window_ms: u64) -> (PointSeries, PointSeries) {
    let mut sorted: Vec<&WindowRecord> = records.to_vec();
    sorted.sort_by_key(|r| r.ts);
    let n = sorted.len();
    if n == 0 {
        return (Vec::new(), Vec::new());
    }
    let mut prompt_prefix = vec![0u64; n + 1];
    let mut hit_prefix = vec![0u64; n + 1];
    let mut drafted_prefix = vec![0u64; n + 1];
    let mut accepted_prefix = vec![0u64; n + 1];
    for i in 0..n {
        prompt_prefix[i + 1] = prompt_prefix[i] + sorted[i].prompt_tokens;
        hit_prefix[i + 1] = hit_prefix[i] + sorted[i].prefix_cache_hit_tokens;
        drafted_prefix[i + 1] = drafted_prefix[i] + sorted[i].drafted_tokens;
        accepted_prefix[i + 1] = accepted_prefix[i] + sorted[i].accepted_tokens;
    }
    let mut cache = Vec::with_capacity(n);
    let mut spec = Vec::with_capacity(n);
    for i in 0..n {
        let ts = sorted[i].ts;
        let lo = ts.saturating_sub(window_ms);
        let start = sorted.partition_point(|r| r.ts < lo);
        let prompt = prompt_prefix[i + 1] - prompt_prefix[start];
        let hit = hit_prefix[i + 1] - hit_prefix[start];
        let drafted = drafted_prefix[i + 1] - drafted_prefix[start];
        let accepted = accepted_prefix[i + 1] - accepted_prefix[start];
        if let Some(rate) = prefix_cache_hit_rate(hit, prompt) {
            cache.push((ts as f64, rate * 100.0));
        }
        if let Some(rate) = speculative_acceptance(accepted, drafted) {
            spec.push((ts as f64, rate * 100.0));
        }
    }
    (cache, spec)
}

/// Extract the chart series for `kind` from the store, filtered to the
/// `[last - window_ms, last]` time window (FR-4.2, R4: timestamp-driven).
pub fn prepare(store: &Store, kind: ChartKind, window_ms: u64) -> ChartData {
    let Some(t_end) = store.max_timestamp_ms() else {
        return ChartData {
            kind,
            domain: (0.0, 1.0),
            series: Vec::new(),
            restarts: Vec::new(),
        };
    };
    let t_end = t_end as f64;
    let t_start = (t_end - window_ms as f64).max(0.0);
    let in_window = |ts: u64| (ts as f64) >= t_start && (ts as f64) <= t_end;
    let restarts = store
        .restarts()
        .iter()
        .copied()
        .filter(|ts| in_window(*ts))
        .map(|ts| ts as f64)
        .collect();
    let domain = (t_start, t_end);
    let mut series = match kind {
        ChartKind::Throughput => {
            let mut prefill = Vec::new();
            let mut decode = Vec::new();
            for s in store
                .throughput()
                .iter()
                .filter(|s| in_window(s.timestamp_ms))
            {
                if let Some(v) = s.prefill_tps {
                    prefill.push((s.timestamp_ms as f64, v));
                }
                if let Some(v) = s.decode_tps {
                    decode.push((s.timestamp_ms as f64, v));
                }
            }
            vec![
                Series {
                    name: "prefill",
                    unit: "tok/s",
                    points: prefill,
                },
                Series {
                    name: "decode",
                    unit: "tok/s",
                    points: decode,
                },
            ]
        }
        ChartKind::Latency => {
            let mut ttft = Vec::new();
            let mut per_token = Vec::new();
            for p in store
                .latency_points()
                .iter()
                .filter(|p| in_window(p.timestamp_ms))
            {
                if let Some(v) = p.ttft_ms {
                    ttft.push((p.timestamp_ms as f64, v));
                }
                if let Some(v) = p.decode_latency_ms {
                    per_token.push((p.timestamp_ms as f64, v));
                }
            }
            vec![
                Series {
                    name: "ttft",
                    unit: "ms",
                    points: ttft,
                },
                Series {
                    name: "per-token",
                    unit: "ms",
                    points: per_token,
                },
            ]
        }
        ChartKind::Cache => {
            // The rates are windowed over a fixed 60 s (WINDOW_MS) anchored at
            // each completed request; the chart window only filters which
            // points are shown.
            let records: Vec<&WindowRecord> = store.request_records().iter().collect();
            let (cache, spec) = cache_spec_series(&records, WINDOW_MS);
            let cache: Vec<(f64, f64)> = cache
                .into_iter()
                .filter(|(t, _)| *t >= t_start && *t <= t_end)
                .collect();
            let spec: Vec<(f64, f64)> = spec
                .into_iter()
                .filter(|(t, _)| *t >= t_start && *t <= t_end)
                .collect();
            vec![
                Series {
                    name: "cache hit",
                    unit: "%",
                    points: cache,
                },
                Series {
                    name: "spec accept",
                    unit: "%",
                    points: spec,
                },
            ]
        }
        ChartKind::Scheduler => {
            let samples: Vec<_> = store
                .throughput()
                .iter()
                .filter(|s| in_window(s.timestamp_ms))
                .collect();
            let running = samples
                .iter()
                .map(|s| {
                    (
                        s.timestamp_ms as f64,
                        s.scheduler_running.unwrap_or(0) as f64,
                    )
                })
                .collect();
            let waiting = samples
                .iter()
                .map(|s| {
                    (
                        s.timestamp_ms as f64,
                        s.scheduler_waiting.unwrap_or(0) as f64,
                    )
                })
                .collect();
            vec![
                Series {
                    name: "active",
                    unit: "",
                    points: running,
                },
                Series {
                    name: "waiting",
                    unit: "",
                    points: waiting,
                },
            ]
        }
    };
    // Points are collected in arrival order; sort by timestamp so line
    // series draw left-to-right even if events arrive out of order.
    for s in &mut series {
        s.points
            .sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    }
    ChartData {
        kind,
        domain,
        series,
        restarts,
    }
}

fn y_max<I: IntoIterator<Item = f64>>(values: I) -> f64 {
    let max = values.into_iter().fold(0.0_f64, f64::max);
    if max <= 0.0 { 1.0 } else { max * 1.1 }
}

/// Y-axis tops for a dual-axis chart. When the two series' maxima are
/// within 2x of each other, both axes share the same scale (the larger
/// top) so the lines are directly comparable; otherwise each axis keeps
/// its own scale.
fn axis_ranges(left_max: f64, right_max: f64) -> (f64, f64) {
    let (lo, hi) = (left_max.min(right_max), left_max.max(right_max));
    if hi <= 2.0 * lo {
        (hi, hi)
    } else {
        (left_max, right_max)
    }
}

fn line_style(color: [u8; 3]) -> ShapeStyle {
    rgb(color).stroke_width(2)
}

fn buf_bytes(size: (u32, u32)) -> usize {
    (size.0 * size.1 * 3) as usize
}

fn render_empty(size: (u32, u32)) -> Vec<u8> {
    let mut buf = vec![0u8; buf_bytes(size)];
    {
        let backend = BitMapBackend::with_buffer(&mut buf, size);
        let root = backend.into_drawing_area();
        root.fill(&BG).unwrap();
        let style = ("sans-serif", 16.0, &TICK)
            .into_text_style(&root)
            .pos(Pos::new(HPos::Center, VPos::Center));
        root.draw_text(
            "no data",
            &style,
            ((size.0 / 2) as i32, (size.1 / 2) as i32),
        )
        .unwrap();
        root.present().unwrap();
    }
    buf
}

/// Render a dual-axis line chart: `series[0]` on the left axis, `series[1]`
/// on the right axis. Line colors are unified across all charts — the
/// left-axis line is always yellow, the right-axis line always blue — and
/// each axis's tick labels use its line's color. When the two series'
/// maxima are within 2x of each other, both axes share the same scale so
/// the lines are directly comparable.
fn render_dual(data: &ChartData, size: (u32, u32)) -> Vec<u8> {
    let mut buf = vec![0u8; buf_bytes(size)];
    // Pad to two series so a single-series chart cannot panic on `series[1]`.
    let padded;
    let data = if data.series.len() < 2 {
        let mut d = data.clone();
        d.series.push(Series {
            name: "",
            unit: "",
            points: Vec::new(),
        });
        padded = d;
        &padded
    } else {
        data
    };
    {
        let backend = BitMapBackend::with_buffer(&mut buf, size);
        let root = backend.into_drawing_area();
        root.fill(&BG).unwrap();
        let left = &data.series[0];
        let right = &data.series[1];
        let left_max = y_max(left.points.iter().map(|&(_, v)| v));
        let right_max = y_max(right.points.iter().map(|&(_, v)| v));
        let (left_top, right_top) = axis_ranges(left_max, right_max);

        let mut chart = ChartBuilder::on(&root)
            .x_label_area_size(0)
            .y_label_area_size(Y_LABEL_AREA)
            .right_y_label_area_size(Y_LABEL_AREA)
            .margin(MARGIN)
            .build_cartesian_2d(data.domain.0..data.domain.1, 0.0..left_top)
            .unwrap()
            .set_secondary_coord(data.domain.0..data.domain.1, 0.0..right_top);

        let left_color = rgb(LEFT_LINE);
        let right_color = rgb(RIGHT_LINE);
        let left_tick = ("sans-serif", 11.0, &left_color).into_text_style(&root);
        let right_tick = ("sans-serif", 11.0, &right_color).into_text_style(&root);

        chart
            .configure_mesh()
            .x_labels(6)
            .y_labels(4)
            .y_label_style(left_tick)
            .axis_style(GRID)
            .light_line_style(GRID)
            .draw()
            .unwrap();
        chart
            .configure_secondary_axes()
            .y_labels(4)
            .label_style(right_tick)
            .axis_style(GRID)
            .draw()
            .unwrap();

        chart
            .draw_series(LineSeries::new(
                left.points.iter().copied(),
                line_style(LEFT_LINE),
            ))
            .unwrap();
        chart
            .draw_secondary_series(LineSeries::new(
                right.points.iter().copied(),
                line_style(RIGHT_LINE),
            ))
            .unwrap();

        let (t0, t1) = data.domain;
        for ts in &data.restarts {
            chart
                .plotting_area()
                .draw(&PathElement::new(
                    vec![(*ts, 0.0), (*ts, left_top)],
                    RESTART.mix(0.8),
                ))
                .unwrap();
            let pixel = chart
                .plotting_area()
                .as_coord_spec()
                .translate(&(*ts, left_top * 0.96));
            let hpos = if *ts >= (t0 + t1) / 2.0 {
                HPos::Right
            } else {
                HPos::Left
            };
            let style = ("sans-serif", 10.0, &RESTART)
                .into_text_style(&root)
                .pos(Pos::new(hpos, VPos::Top));
            root.draw_text("server restarted", &style, pixel).unwrap();
        }

        root.present().unwrap();
    }
    buf
}

/// Render a prepared chart into an RGB image buffer (FR-4.1).
pub fn render(data: &ChartData, size: (u32, u32)) -> Vec<u8> {
    if data.series.iter().all(|s| s.points.is_empty()) {
        return render_empty(size);
    }
    render_dual(data, size)
}

fn to_image(rgb: &[u8], size: (u32, u32)) -> slint::Image {
    let mut buf = slint::SharedPixelBuffer::<slint::Rgb8Pixel>::new(size.0, size.1);
    buf.make_mut_bytes().copy_from_slice(rgb);
    slint::Image::from_rgb8(buf)
}

/// Render all four charts for the store within `window_ms`, each at its own
/// panel size (FR-4.2).
pub fn render_all_sized(store: &Store, window_ms: u64, sizes: &[(u32, u32); 4]) -> ChartImages {
    let data = [
        prepare(store, ChartKind::Throughput, window_ms),
        prepare(store, ChartKind::Latency, window_ms),
        prepare(store, ChartKind::Cache, window_ms),
        prepare(store, ChartKind::Scheduler, window_ms),
    ];
    let rendered = [
        render(&data[0], sizes[0]),
        render(&data[1], sizes[1]),
        render(&data[2], sizes[2]),
        render(&data[3], sizes[3]),
    ];
    ChartImages {
        throughput: to_image(&rendered[0], sizes[0]),
        latency: to_image(&rendered[1], sizes[1]),
        cache: to_image(&rendered[2], sizes[2]),
        scheduler: to_image(&rendered[3], sizes[3]),
    }
}

/// Fingerprint of the chart-relevant store state, used to skip re-renders
/// when nothing changed (FR-4.1: re-render only on data change).
///
/// Hashes the length, oldest point, and newest point of each series plus the
/// domain end, so a change is detected even when a buffer is full and the
/// newest timestamp does not advance.
pub fn fingerprint(store: &Store) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    store.max_timestamp_ms().hash(&mut h);
    let throughput = store.throughput();
    throughput.len().hash(&mut h);
    for s in [throughput.first(), throughput.last()]
        .into_iter()
        .flatten()
    {
        s.timestamp_ms.hash(&mut h);
        s.decode_tps.map(|v| v.to_bits()).hash(&mut h);
        s.prefill_tps.map(|v| v.to_bits()).hash(&mut h);
        s.scheduler_running.hash(&mut h);
        s.scheduler_waiting.hash(&mut h);
    }
    let latency = store.latency_points();
    latency.len().hash(&mut h);
    for p in [latency.first(), latency.last()].into_iter().flatten() {
        p.timestamp_ms.hash(&mut h);
        p.ttft_ms.map(|v| v.to_bits()).hash(&mut h);
        p.decode_latency_ms.map(|v| v.to_bits()).hash(&mut h);
    }
    let records = store.request_records();
    records.len().hash(&mut h);
    for r in [records.first(), records.last()].into_iter().flatten() {
        r.ts.hash(&mut h);
        r.prompt_tokens.hash(&mut h);
        r.prefix_cache_hit_tokens.hash(&mut h);
        r.drafted_tokens.hash(&mut h);
        r.accepted_tokens.hash(&mut h);
    }
    store.restarts().hash(&mut h);
    h.finish()
}

#[macro_export]
macro_rules! apply_chart_images {
    ($window:expr, $images:expr) => {{
        $window.set_chart_throughput_image($images.throughput.clone());
        $window.set_chart_latency_image($images.latency.clone());
        $window.set_chart_cache_image($images.cache.clone());
        $window.set_chart_scheduler_image($images.scheduler.clone());
    }};
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{ParsedEvent, parse_line};

    fn throughput_line(ts: u64, decode: f64, prefill: f64) -> ParsedEvent {
        let raw = format!(
            r#"{{"artifact_type":"ninfer_serve_request_log","schema_version":20,"server_instance_id":"s","timestamp_unix_ms":{ts},"event":"throughput","throughput_tokens_per_second":{{"decode":{decode},"prefill":{prefill}}},"scheduler":{{"running":1,"prefilling":0,"waiting":2}},"context_cache":{{"occupancy":{{"device_main_kv_pages":10,"device_backend_kv_pages":11,"host_kv_bytes":100}}}}}}"#
        );
        parse_line(raw.as_bytes()).unwrap()
    }

    #[allow(clippy::too_many_arguments)]
    fn request_done_line(
        ts: u64,
        id: u64,
        ttft: f64,
        decode: f64,
        completion: u64,
        prompt: u64,
        hit: u64,
        drafted: u64,
        accepted: u64,
    ) -> ParsedEvent {
        let raw = format!(
            r#"{{"artifact_type":"ninfer_serve_request_log","schema_version":20,"server_instance_id":"s","timestamp_unix_ms":{ts},"event":"request_done","request":{{"request_id":{id}}},"result":{{"prompt_tokens":{prompt},"completion_tokens":{completion},"prefix_cache_hit_tokens":{hit}}},"timings_seconds":{{"ttft":{ttft},"decode":{decode}}},"speculative":{{"drafted_tokens":{drafted},"accepted_tokens":{accepted}}}}}"#
        );
        parse_line(raw.as_bytes()).unwrap()
    }

    #[test]
    fn prepare_empty_store_has_no_points() {
        let store = Store::new();
        for kind in [
            ChartKind::Throughput,
            ChartKind::Latency,
            ChartKind::Cache,
            ChartKind::Scheduler,
        ] {
            let data = prepare(&store, kind, 300_000);
            assert!(data.series.iter().all(|s| s.points.is_empty()));
        }
    }

    #[test]
    fn prepare_filters_to_window() {
        let mut store = Store::new();
        store.apply(&throughput_line(0, 1.0, 2.0));
        store.apply(&throughput_line(100_000, 3.0, 4.0));
        store.apply(&throughput_line(200_000, 5.0, 6.0));
        let data = prepare(&store, ChartKind::Throughput, 150_000);
        // Window [50_000, 200_000]: keeps ts=100_000 and ts=200_000.
        assert_eq!(data.series[0].points.len(), 2);
        assert_eq!(data.domain, (50_000.0, 200_000.0));
    }

    #[test]
    fn latency_chart_has_ttft_and_per_token() {
        let mut store = Store::new();
        // ttft = 0.5 s -> 500 ms; decode = 1.0 s over 10 tokens -> 100 ms/token.
        store.apply(&request_done_line(1_000, 1, 0.5, 1.0, 10, 100, 25, 10, 5));
        let data = prepare(&store, ChartKind::Latency, 300_000);
        assert_eq!(data.series.len(), 2);
        assert_eq!(data.series[0].name, "ttft");
        assert_eq!(data.series[0].points, vec![(1_000.0, 500.0)]);
        assert_eq!(data.series[1].name, "per-token");
        assert_eq!(data.series[1].points, vec![(1_000.0, 100.0)]);
    }

    #[test]
    fn cache_chart_computes_windowed_rates() {
        let mut store = Store::new();
        // prompt 100, hit 25 -> 25% cache hit; drafted 10, accepted 5 -> 50% spec.
        store.apply(&request_done_line(1_000, 1, 0.5, 1.0, 10, 100, 25, 10, 5));
        store.apply(&request_done_line(2_000, 2, 0.5, 1.0, 10, 100, 25, 10, 5));
        let data = prepare(&store, ChartKind::Cache, 300_000);
        assert_eq!(data.series.len(), 2);
        assert_eq!(data.series[0].name, "cache hit");
        assert_eq!(
            data.series[0].points,
            vec![(1_000.0, 25.0), (2_000.0, 25.0)]
        );
        assert_eq!(data.series[1].name, "spec accept");
        assert_eq!(
            data.series[1].points,
            vec![(1_000.0, 50.0), (2_000.0, 50.0)]
        );
    }

    #[test]
    fn cache_chart_uses_fixed_60s_window_regardless_of_chart_window() {
        let mut store = Store::new();
        // Two completed requests 100 s apart: the second one's 60 s window
        // excludes the first, so its rate is computed from itself alone.
        store.apply(&request_done_line(1_000, 1, 0.5, 1.0, 10, 100, 25, 10, 5));
        store.apply(&request_done_line(101_000, 2, 0.5, 1.0, 10, 100, 50, 10, 5));
        // A wide chart window keeps both points, but the second point's rate
        // must still be 50% (its own 60 s window), not 37.5% (both requests).
        let data = prepare(&store, ChartKind::Cache, 300_000);
        assert_eq!(
            data.series[0].points,
            vec![(1_000.0, 25.0), (101_000.0, 50.0)]
        );
    }

    #[test]
    fn latency_chart_per_token_from_raw_json() {
        let mut store = Store::new();
        // decode = 2.0 s over 20 completion tokens -> 100 ms/token.
        store.apply(&request_done_line(1_000, 1, 0.5, 2.0, 20, 100, 25, 10, 5));
        let data = prepare(&store, ChartKind::Latency, 300_000);
        assert_eq!(data.series[1].name, "per-token");
        assert_eq!(data.series[1].points, vec![(1_000.0, 100.0)]);
    }

    #[test]
    fn scheduler_chart_has_active_and_waiting() {
        let mut store = Store::new();
        store.apply(&throughput_line(1_000, 1.0, 2.0));
        let data = prepare(&store, ChartKind::Scheduler, 300_000);
        assert_eq!(data.series.len(), 2);
        assert_eq!(data.series[0].name, "active");
        assert_eq!(data.series[0].points, vec![(1_000.0, 1.0)]);
        assert_eq!(data.series[1].name, "waiting");
        assert_eq!(data.series[1].points, vec![(1_000.0, 2.0)]);
    }

    // TODO(test): wall-clock timing assertion can be flaky on loaded CI;
    // consider a higher threshold or a median-of-N measurement.
    #[test]
    fn render_cost_under_50ms() {
        let mut store = Store::new();
        for ts in 0..1000 {
            store.apply(&throughput_line(ts * 5_000, 10.0 + (ts % 50) as f64, 20.0));
        }
        for kind in [
            ChartKind::Throughput,
            ChartKind::Latency,
            ChartKind::Cache,
            ChartKind::Scheduler,
        ] {
            let data = prepare(&store, kind, 3_600_000);
            let start = std::time::Instant::now();
            let _ = render(&data, (CHART_WIDTH, CHART_HEIGHT));
            let elapsed = start.elapsed();
            assert!(
                elapsed < std::time::Duration::from_millis(50),
                "{kind:?} took {elapsed:?}"
            );
        }
    }

    #[test]
    fn render_empty_draws_no_data() {
        let store = Store::new();
        let data = prepare(&store, ChartKind::Throughput, 300_000);
        let rendered = render(&data, (CHART_WIDTH, CHART_HEIGHT));
        assert_eq!(rendered.len(), buf_bytes((CHART_WIDTH, CHART_HEIGHT)));
    }

    #[test]
    fn fingerprint_is_deterministic_and_changes_on_data() {
        let mut store = Store::new();
        let f0 = fingerprint(&store);
        assert_eq!(f0, fingerprint(&store), "fingerprint must be deterministic");
        store.apply(&throughput_line(1_000, 1.0, 2.0));
        let f1 = fingerprint(&store);
        assert_ne!(f0, f1, "fingerprint must change when data changes");
        assert_eq!(f1, fingerprint(&store));
        store.apply(&throughput_line(2_000, 3.0, 4.0));
        assert_ne!(f1, fingerprint(&store));
    }

    #[test]
    fn fingerprint_changes_when_buffer_full_and_ts_does_not_advance() {
        let mut store = Store::new();
        // Fill the throughput ring buffer to capacity.
        for ts in 0..crate::metrics::MAX_SERIES_POINTS {
            store.apply(&throughput_line(ts as u64, 1.0, 2.0));
        }
        let before = fingerprint(&store);
        // A new point with the same (max) timestamp evicts the oldest; the
        // fingerprint must still change even though max_timestamp_ms is
        // unchanged.
        let max_ts = crate::metrics::MAX_SERIES_POINTS as u64 - 1;
        store.apply(&throughput_line(max_ts, 9.0, 9.0));
        assert_ne!(before, fingerprint(&store));
    }

    #[test]
    fn fingerprint_changes_when_duplicated_newest_point_evicts_oldest() {
        let mut store = Store::new();
        for ts in 0..crate::metrics::MAX_SERIES_POINTS {
            store.apply(&throughput_line(ts as u64, 1.0, 2.0));
        }
        let before = fingerprint(&store);
        let max_ts = crate::metrics::MAX_SERIES_POINTS as u64 - 1;
        store.apply(&throughput_line(max_ts, 1.0, 2.0));
        assert_ne!(before, fingerprint(&store));
    }

    #[test]
    fn fingerprint_changes_on_request_records() {
        let mut store = Store::new();
        let before = fingerprint(&store);
        store.apply(&request_done_line(1_000, 1, 0.5, 1.0, 10, 100, 25, 10, 5));
        assert_ne!(before, fingerprint(&store));
    }

    #[test]
    fn prepare_sorts_out_of_order_points() {
        let mut store = Store::new();
        store.apply(&throughput_line(200_000, 3.0, 3.0));
        store.apply(&throughput_line(0, 1.0, 1.0));
        store.apply(&throughput_line(100_000, 2.0, 2.0));
        let data = prepare(&store, ChartKind::Throughput, 300_000);
        let ts: Vec<f64> = data.series[0].points.iter().map(|&(t, _)| t).collect();
        assert_eq!(
            ts,
            vec![0.0, 100_000.0, 200_000.0],
            "points must be sorted by ts"
        );
    }

    #[test]
    fn restart_marker_renders_purple_pixels() {
        let mut store = Store::new();
        let raw1 = r#"{"artifact_type":"ninfer_serve_request_log","schema_version":20,"server_instance_id":"s1","timestamp_unix_ms":1000,"event":"server_start"}"#;
        store.apply(&parse_line(raw1.as_bytes()).unwrap());
        store.apply(&throughput_line(2000, 10.0, 20.0));
        let raw2 = r#"{"artifact_type":"ninfer_serve_request_log","schema_version":20,"server_instance_id":"s2","timestamp_unix_ms":5000,"event":"server_start"}"#;
        store.apply(&parse_line(raw2.as_bytes()).unwrap());
        store.apply(&throughput_line(6000, 30.0, 40.0));
        let data = prepare(&store, ChartKind::Throughput, 10_000);
        let rendered = render(&data, (CHART_WIDTH, CHART_HEIGHT));
        // The restart line/label use the purple RESTART color; at least one
        // pixel should be close to it.
        let (r, g, b) = (RESTART.0, RESTART.1, RESTART.2);
        let found = rendered.chunks_exact(3).any(|px| {
            (px[0] as i32 - r as i32).abs() <= 32
                && (px[1] as i32 - g as i32).abs() <= 32
                && (px[2] as i32 - b as i32).abs() <= 32
        });
        assert!(found, "no purple restart-marker pixels found");
    }

    #[test]
    fn y_axis_labels_use_series_colors() {
        let mut store = Store::new();
        store.apply(&throughput_line(1_000, 10.0, 20.0));
        store.apply(&throughput_line(2_000, 30.0, 40.0));
        let data = prepare(&store, ChartKind::Throughput, 3_000);
        let rendered = render(&data, (CHART_WIDTH, CHART_HEIGHT));
        let w = CHART_WIDTH as usize;
        let h = CHART_HEIGHT as usize;
        let close = |i: usize, t: [u8; 3]| {
            (rendered[i] as i32 - t[0] as i32).abs() <= 32
                && (rendered[i + 1] as i32 - t[1] as i32).abs() <= 32
                && (rendered[i + 2] as i32 - t[2] as i32).abs() <= 32
        };
        let in_column = |x0: usize, x1: usize, t: [u8; 3]| {
            (x0..x1).any(|x| (0..h).any(|y| close((y * w + x) * 3, t)))
        };
        assert!(
            in_column(0, Y_LABEL_AREA as usize, LEFT_LINE),
            "left y-axis labels must use the left line color"
        );
        assert!(
            in_column(w - Y_LABEL_AREA as usize, w, RIGHT_LINE),
            "right y-axis labels must use the right line color"
        );
    }

    #[test]
    fn axis_ranges_share_scale_within_2x() {
        assert_eq!(axis_ranges(11.0, 13.2), (13.2, 13.2));
        assert_eq!(axis_ranges(13.2, 11.0), (13.2, 13.2));
        assert_eq!(axis_ranges(10.0, 20.0), (20.0, 20.0));
        assert_eq!(axis_ranges(7.0, 7.0), (7.0, 7.0));
    }

    #[test]
    fn axis_ranges_keep_own_scale_beyond_2x() {
        assert_eq!(axis_ranges(11.0, 55.0), (11.0, 55.0));
        assert_eq!(axis_ranges(55.0, 11.0), (55.0, 11.0));
        assert_eq!(axis_ranges(1.0, 100.0), (1.0, 100.0));
    }

    /// Pixel position of value `v` on an axis with top `top`, given the
    /// plot geometry of a `CHART_WIDTH x CHART_HEIGHT` render.
    fn y_pixel(v: f64, top: f64) -> usize {
        let bottom = (CHART_HEIGHT - MARGIN) as usize;
        let height = (CHART_HEIGHT - 2 * MARGIN) as usize;
        bottom - ((v / top) * height as f64).round() as usize
    }

    #[test]
    fn shared_scale_when_maxes_within_2x() {
        let mut store = Store::new();
        store.apply(&throughput_line(0, 10.0, 10.0));
        store.apply(&throughput_line(1_000, 12.0, 5.0));
        let data = prepare(&store, ChartKind::Throughput, 3_000);
        let rendered = render(&data, (CHART_WIDTH, CHART_HEIGHT));
        let w = CHART_WIDTH as usize;
        let plot_right = (CHART_WIDTH - MARGIN - Y_LABEL_AREA) as usize;
        // Maxima 10 vs 12 are within 2x, so both axes share the top 12 * 1.1.
        let top = 12.0 * 1.1;
        let close = |i: usize, t: [u8; 3]| {
            (rendered[i] as i32 - t[0] as i32).abs() <= 32
                && (rendered[i + 1] as i32 - t[1] as i32).abs() <= 32
                && (rendered[i + 2] as i32 - t[2] as i32).abs() <= 32
        };
        let in_band = |x0: usize, x1: usize, y0: usize, y1: usize, t: [u8; 3]| {
            (x0..x1).any(|x| (y0..=y1).any(|y| close((y * w + x) * 3, t)))
        };
        // At t=1000 (right plot edge): prefill 5, decode 12 on the shared scale.
        let y_prefill = y_pixel(5.0, top);
        let y_decode = y_pixel(12.0, top);
        assert!(
            in_band(
                plot_right - 12,
                plot_right + 1,
                y_prefill - 10,
                y_prefill + 10,
                LEFT_LINE
            ),
            "prefill line must sit at the shared-scale height"
        );
        assert!(
            in_band(
                plot_right - 12,
                plot_right + 1,
                y_decode - 10,
                y_decode + 10,
                RIGHT_LINE
            ),
            "decode line must sit at the shared-scale height"
        );
    }

    #[test]
    fn independent_scales_when_maxes_beyond_2x() {
        let mut store = Store::new();
        store.apply(&throughput_line(0, 10.0, 10.0));
        store.apply(&throughput_line(1_000, 50.0, 5.0));
        let data = prepare(&store, ChartKind::Throughput, 3_000);
        let rendered = render(&data, (CHART_WIDTH, CHART_HEIGHT));
        let w = CHART_WIDTH as usize;
        let plot_left = (MARGIN + Y_LABEL_AREA) as usize;
        // Maxima 10 vs 50 are beyond 2x, so each axis keeps its own top.
        let left_top = 10.0 * 1.1;
        let right_top = 50.0 * 1.1;
        let close = |i: usize, t: [u8; 3]| {
            (rendered[i] as i32 - t[0] as i32).abs() <= 32
                && (rendered[i + 1] as i32 - t[1] as i32).abs() <= 32
                && (rendered[i + 2] as i32 - t[2] as i32).abs() <= 32
        };
        let in_band = |x0: usize, x1: usize, y0: usize, y1: usize, t: [u8; 3]| {
            (x0..x1).any(|x| (y0..=y1).any(|y| close((y * w + x) * 3, t)))
        };
        // At t=0 (left plot edge) both lines are at value 10, but on
        // different scales: prefill near the top, decode near the bottom.
        let y_prefill = y_pixel(10.0, left_top);
        let y_decode = y_pixel(10.0, right_top);
        assert!(
            in_band(
                plot_left,
                plot_left + 12,
                y_prefill - 10,
                y_prefill + 10,
                LEFT_LINE
            ),
            "prefill line must sit at its own-scale height"
        );
        assert!(
            in_band(
                plot_left,
                plot_left + 12,
                y_decode - 10,
                y_decode + 10,
                RIGHT_LINE
            ),
            "decode line must sit at its own-scale height"
        );
    }
}
