// NInferMonitor
//
// This is free software; see the LICENSE.md file in the source distribution for precise wording.
//
// Copyright (C) 2026 Aleksey Sanin aleksey@aleksey.com. All Rights Reserved.

use std::collections::VecDeque;

use crate::parser::{RequestDoneEvent, ThroughputEvent};

/// Maximum number of points kept per time series (FR-4.4).
pub const MAX_SERIES_POINTS: usize = 10_000;

/// Length of the sliding window used by the windowed KPIs (FR-3.1).
pub const WINDOW_MS: u64 = 60_000;

/// Retention for the sliding window: two 60 s windows, so the previous
/// window can be computed for trend indicators (FR-3.2).
const RETENTION_MS: u64 = 2 * WINDOW_MS;

/// Size cap for the sliding window's per-request records and error
/// timestamps. `Window` prunes by age (`advance`) only, so a burst of many
/// completions within the retention range (e.g. a dense or backfilled log)
/// would otherwise be retained unboundedly, making `advance` O(n²) and
/// spiking memory. Capping the deques bounds both; the cap is far above any
/// realistic 120 s request count, so legitimate KPIs are unaffected.
const MAX_WINDOW_RECORDS: usize = 100_000;

/// One point in a throughput / scheduler / occupancy time series.
#[derive(Debug, Clone, PartialEq)]
pub struct ThroughputSample {
    pub timestamp_ms: u64,
    pub decode_tps: Option<f64>,
    pub prefill_tps: Option<f64>,
    pub scheduler_running: Option<u64>,
    pub scheduler_prefilling: Option<u64>,
    pub scheduler_waiting: Option<u64>,
    pub device_main_kv_pages: Option<u64>,
    pub device_backend_kv_pages: Option<u64>,
    pub host_kv_bytes: Option<u64>,
}

impl ThroughputSample {
    pub fn from_event(e: &ThroughputEvent) -> Self {
        Self {
            timestamp_ms: e.timestamp_unix_ms.unwrap_or(0),
            decode_tps: e.throughput_tokens_per_second.decode,
            prefill_tps: e.throughput_tokens_per_second.prefill,
            scheduler_running: e.scheduler.running,
            scheduler_prefilling: e.scheduler.prefilling,
            scheduler_waiting: e.scheduler.waiting,
            device_main_kv_pages: e.context_cache.occupancy.device_main_kv_pages,
            device_backend_kv_pages: e.context_cache.occupancy.device_backend_kv_pages,
            host_kv_bytes: e.context_cache.occupancy.host_kv_bytes,
        }
    }
}

/// One point in the latency time series (Latency widget): TTFT and
/// per-token decode latency for a completed request, in milliseconds.
#[derive(Debug, Clone, PartialEq)]
pub struct LatencyPoint {
    pub timestamp_ms: u64,
    pub ttft_ms: Option<f64>,
    pub decode_latency_ms: Option<f64>,
}

/// Bounded FIFO ring buffer for a time series (FR-4.4).
#[derive(Debug, Clone)]
pub struct RingBuffer<T> {
    buf: VecDeque<T>,
    capacity: usize,
}

impl<T> RingBuffer<T> {
    pub fn new(capacity: usize) -> Self {
        Self {
            buf: VecDeque::new(),
            capacity,
        }
    }

    pub fn push(&mut self, item: T) {
        if self.capacity == 0 {
            return;
        }
        if self.buf.len() >= self.capacity {
            self.buf.pop_front();
        }
        self.buf.push_back(item);
    }

    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn first(&self) -> Option<&T> {
        self.buf.front()
    }

    pub fn last(&self) -> Option<&T> {
        self.buf.back()
    }

    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.buf.iter()
    }

    pub fn clear(&mut self) {
        self.buf.clear()
    }
}

/// Per-completed-request contribution to the windowed KPIs.
#[derive(Debug, Clone)]
pub struct WindowRecord {
    pub ts: u64,
    pub ttft: Option<f64>,
    pub decode_latency_per_token: Option<f64>,
    pub prompt_tokens: u64,
    pub prefix_cache_hit_tokens: u64,
    pub drafted_tokens: u64,
    pub accepted_tokens: u64,
}

impl WindowRecord {
    pub fn from_done(e: &RequestDoneEvent) -> Self {
        Self {
            ts: e.timestamp_unix_ms.unwrap_or(0),
            ttft: e.timings_seconds.ttft,
            decode_latency_per_token: decode_latency_per_token(
                e.timings_seconds.decode,
                e.result.completion_tokens,
            ),
            prompt_tokens: e.result.prompt_tokens.unwrap_or(0),
            prefix_cache_hit_tokens: e.result.prefix_cache_hit_tokens.unwrap_or(0),
            drafted_tokens: e.speculative.drafted_tokens.unwrap_or(0),
            accepted_tokens: e.speculative.accepted_tokens.unwrap_or(0),
        }
    }
}

/// Sliding 60 s window over completed requests and errors (FR-3.1).
///
/// The window is anchored to the newest event timestamp seen, so backfilled
/// historical logs yield the last 60 s of the log rather than an empty window.
/// Entries are retained for two windows (`RETENTION_MS`) so the previous 60 s
/// window can be aggregated for trend indicators (FR-3.2); the current-window
/// accessors filter to the most recent `WINDOW_MS`.
///
/// In addition to the age-based pruning in `advance`, the record and error
/// deques are size-capped at `MAX_WINDOW_RECORDS` (oldest evicted first), so a
/// dense burst of events within the retention range cannot grow them
/// without bound.
#[derive(Debug, Clone, Default)]
pub struct Window {
    records: VecDeque<WindowRecord>,
    errors: VecDeque<u64>,
    anchor: Option<u64>,
}

impl Window {
    /// Move the window anchor forward to `ts` and drop entries that fell out
    /// of the retention range.
    ///
    /// Uses a full scan (not front-popping) so out-of-order entries are
    /// evicted regardless of their position in the deque.
    pub fn advance(&mut self, ts: u64) {
        let anchor = self.anchor.map_or(ts, |a| a.max(ts));
        self.anchor = Some(anchor);
        let cutoff = anchor.saturating_sub(RETENTION_MS);
        // TODO(perf): full scan of records/errors on every event; use an
        // index or time-ordered queue if this becomes measurable.
        self.records.retain(|r| r.ts >= cutoff);
        self.errors.retain(|&ts| ts >= cutoff);
    }

    fn in_current(&self, ts: u64) -> bool {
        self.anchor
            .is_some_and(|a| ts >= a.saturating_sub(WINDOW_MS))
    }

    /// The previous 60 s window is `[anchor - RETENTION_MS, anchor - WINDOW_MS)`.
    fn in_previous(&self, ts: u64) -> bool {
        self.anchor.is_some_and(|a| {
            let hi = a.saturating_sub(WINDOW_MS);
            let lo = a.saturating_sub(RETENTION_MS);
            ts >= lo && ts < hi
        })
    }

    pub fn add_request(&mut self, record: WindowRecord) {
        self.records.push_back(record);
        if self.records.len() > MAX_WINDOW_RECORDS {
            self.records.pop_front();
        }
    }

    pub fn add_error(&mut self, ts: u64) {
        self.errors.push_back(ts);
        if self.errors.len() > MAX_WINDOW_RECORDS {
            self.errors.pop_front();
        }
    }

    pub fn anchor(&self) -> Option<u64> {
        self.anchor
    }

    pub fn cutoff(&self) -> Option<u64> {
        self.anchor.map(|a| a.saturating_sub(WINDOW_MS))
    }

    pub fn range(&self) -> Option<(u64, u64)> {
        self.anchor.map(|a| (a.saturating_sub(WINDOW_MS), a))
    }

    /// Number of completed requests in the current 60 s window.
    pub fn request_count(&self) -> usize {
        self.records
            .iter()
            .filter(|r| self.in_current(r.ts))
            .count()
    }

    /// Number of errors in the current 60 s window.
    pub fn error_count(&self) -> usize {
        self.errors
            .iter()
            .filter(|&&ts| self.in_current(ts))
            .count()
    }

    /// Mean TTFT over completed requests in the window (FR-3.1).
    pub fn avg_ttft(&self) -> Option<f64> {
        let values: Vec<f64> = self
            .records
            .iter()
            .filter(|r| self.in_current(r.ts))
            .filter_map(|r| r.ttft)
            .collect();
        mean(&values)
    }

    /// Mean decode latency per token over completed requests (FR-3.1).
    pub fn avg_decode_latency_per_token(&self) -> Option<f64> {
        let values: Vec<f64> = self
            .records
            .iter()
            .filter(|r| self.in_current(r.ts))
            .filter_map(|r| r.decode_latency_per_token)
            .collect();
        mean(&values)
    }

    /// `sum(prefix_cache_hit_tokens) / sum(prompt_tokens)` in the window.
    pub fn prefix_cache_hit_rate(&self) -> Option<f64> {
        let records: Vec<&WindowRecord> = self
            .records
            .iter()
            .filter(|r| self.in_current(r.ts))
            .collect();
        let prompt: u64 = records.iter().map(|r| r.prompt_tokens).sum();
        let hit: u64 = records.iter().map(|r| r.prefix_cache_hit_tokens).sum();
        prefix_cache_hit_rate(hit, prompt)
    }

    /// `sum(accepted_tokens) / sum(drafted_tokens)` in the window.
    pub fn speculative_acceptance(&self) -> Option<f64> {
        let records: Vec<&WindowRecord> = self
            .records
            .iter()
            .filter(|r| self.in_current(r.ts))
            .collect();
        let drafted: u64 = records.iter().map(|r| r.drafted_tokens).sum();
        let accepted: u64 = records.iter().map(|r| r.accepted_tokens).sum();
        speculative_acceptance(accepted, drafted)
    }

    /// Number of completed requests in the previous 60 s window (FR-3.2).
    pub fn request_count_previous(&self) -> usize {
        self.records
            .iter()
            .filter(|r| self.in_previous(r.ts))
            .count()
    }

    /// Number of errors in the previous 60 s window (FR-3.2).
    pub fn error_count_previous(&self) -> usize {
        self.errors
            .iter()
            .filter(|&&ts| self.in_previous(ts))
            .count()
    }

    /// Mean TTFT over completed requests in the previous 60 s window (FR-3.2).
    pub fn avg_ttft_previous(&self) -> Option<f64> {
        let values: Vec<f64> = self
            .records
            .iter()
            .filter(|r| self.in_previous(r.ts))
            .filter_map(|r| r.ttft)
            .collect();
        mean(&values)
    }

    /// Mean decode latency per token over the previous 60 s window (FR-3.2).
    pub fn avg_decode_latency_per_token_previous(&self) -> Option<f64> {
        let values: Vec<f64> = self
            .records
            .iter()
            .filter(|r| self.in_previous(r.ts))
            .filter_map(|r| r.decode_latency_per_token)
            .collect();
        mean(&values)
    }

    /// `sum(prefix_cache_hit_tokens) / sum(prompt_tokens)` in the previous
    /// 60 s window (FR-3.2).
    pub fn prefix_cache_hit_rate_previous(&self) -> Option<f64> {
        let records: Vec<&WindowRecord> = self
            .records
            .iter()
            .filter(|r| self.in_previous(r.ts))
            .collect();
        let prompt: u64 = records.iter().map(|r| r.prompt_tokens).sum();
        let hit: u64 = records.iter().map(|r| r.prefix_cache_hit_tokens).sum();
        prefix_cache_hit_rate(hit, prompt)
    }

    /// `sum(accepted_tokens) / sum(drafted_tokens)` in the previous 60 s
    /// window (FR-3.2).
    pub fn speculative_acceptance_previous(&self) -> Option<f64> {
        let records: Vec<&WindowRecord> = self
            .records
            .iter()
            .filter(|r| self.in_previous(r.ts))
            .collect();
        let drafted: u64 = records.iter().map(|r| r.drafted_tokens).sum();
        let accepted: u64 = records.iter().map(|r| r.accepted_tokens).sum();
        speculative_acceptance(accepted, drafted)
    }

    pub fn clear(&mut self) {
        self.records.clear();
        self.errors.clear();
        self.anchor = None;
    }
}

/// `timings_seconds.decode / result.completion_tokens` (FR-3.1).
pub fn decode_latency_per_token(
    decode_seconds: Option<f64>,
    completion_tokens: Option<u64>,
) -> Option<f64> {
    let decode = decode_seconds?;
    let tokens = completion_tokens?;
    if tokens == 0 {
        return None;
    }
    Some(decode / tokens as f64)
}

/// `sum(prefix_cache_hit_tokens) / sum(prompt_tokens)` (FR-3.1).
pub fn prefix_cache_hit_rate(prefix_hit_tokens: u64, prompt_tokens: u64) -> Option<f64> {
    if prompt_tokens == 0 {
        return None;
    }
    Some(prefix_hit_tokens as f64 / prompt_tokens as f64)
}

/// `sum(speculative.accepted_tokens) / sum(speculative.drafted_tokens)` (FR-3.1).
pub fn speculative_acceptance(accepted_tokens: u64, drafted_tokens: u64) -> Option<f64> {
    if drafted_tokens == 0 {
        return None;
    }
    Some(accepted_tokens as f64 / drafted_tokens as f64)
}

/// Arithmetic mean of `values`; `None` when empty so the UI can show "—" (FR-3.3).
pub fn mean(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    Some(values.iter().sum::<f64>() / values.len() as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_buffer_bounded_fifo() {
        let mut rb: RingBuffer<u32> = RingBuffer::new(3);
        for i in 0..10 {
            rb.push(i);
        }
        assert_eq!(rb.len(), 3);
        assert_eq!(rb.capacity(), 3);
        let items: Vec<u32> = rb.iter().copied().collect();
        assert_eq!(items, vec![7, 8, 9]);
        assert_eq!(rb.last(), Some(&9));
    }

    #[test]
    fn ring_buffer_under_capacity_keeps_all() {
        let mut rb: RingBuffer<u32> = RingBuffer::new(5);
        for i in 0..3 {
            rb.push(i);
        }
        assert_eq!(rb.len(), 3);
        let items: Vec<u32> = rb.iter().copied().collect();
        assert_eq!(items, vec![0, 1, 2]);
    }

    #[test]
    fn ring_buffer_empty() {
        let rb: RingBuffer<u32> = RingBuffer::new(4);
        assert!(rb.is_empty());
        assert_eq!(rb.len(), 0);
        assert_eq!(rb.last(), None);
    }

    #[test]
    fn ring_buffer_clear() {
        let mut rb: RingBuffer<u32> = RingBuffer::new(4);
        rb.push(1);
        rb.push(2);
        rb.clear();
        assert!(rb.is_empty());
    }

    #[test]
    fn mean_empty_is_none() {
        assert_eq!(mean(&[]), None);
    }

    #[test]
    fn mean_of_values() {
        assert_eq!(mean(&[1.0, 2.0, 3.0]), Some(2.0));
        assert_eq!(mean(&[0.5]), Some(0.5));
    }

    #[test]
    fn decode_latency_per_token_divides() {
        assert_eq!(decode_latency_per_token(Some(1.0), Some(4)), Some(0.25));
    }

    #[test]
    fn decode_latency_per_token_zero_tokens_is_none() {
        assert_eq!(decode_latency_per_token(Some(1.0), Some(0)), None);
        assert_eq!(decode_latency_per_token(Some(1.0), None), None);
        assert_eq!(decode_latency_per_token(None, Some(4)), None);
    }

    #[test]
    fn prefix_cache_hit_rate_computes() {
        assert_eq!(prefix_cache_hit_rate(1, 4), Some(0.25));
        assert_eq!(prefix_cache_hit_rate(0, 0), None);
    }

    #[test]
    fn speculative_acceptance_computes() {
        assert_eq!(speculative_acceptance(3, 4), Some(0.75));
        assert_eq!(speculative_acceptance(3, 0), None);
    }

    fn record(ts: u64, ttft: Option<f64>) -> WindowRecord {
        WindowRecord {
            ts,
            ttft,
            decode_latency_per_token: None,
            prompt_tokens: 0,
            prefix_cache_hit_tokens: 0,
            drafted_tokens: 0,
            accepted_tokens: 0,
        }
    }

    #[test]
    fn window_keeps_last_60s() {
        let mut w = Window::default();
        w.add_request(record(0, Some(1.0)));
        w.advance(0);
        w.add_request(record(30_000, Some(3.0)));
        w.advance(30_000);
        // Anchor at 60_000: the ts=0 record is exactly at the cutoff and is
        // kept (cutoff is exclusive), ts=30_000 is kept.
        w.advance(60_000);
        assert_eq!(w.request_count(), 2);
        // Advance to 61_000: ts=0 is now below the cutoff and dropped.
        w.advance(61_000);
        assert_eq!(w.request_count(), 1);
        assert_eq!(w.avg_ttft(), Some(3.0));
    }

    #[test]
    fn window_anchor_is_monotonic() {
        let mut w = Window::default();
        w.add_request(record(100_000, Some(1.0)));
        w.advance(100_000);
        // An older timestamp must not move the anchor backwards.
        w.advance(10);
        assert_eq!(w.anchor(), Some(100_000));
        assert_eq!(w.request_count(), 1);
    }

    #[test]
    fn window_error_count() {
        let mut w = Window::default();
        w.add_error(0);
        w.advance(0);
        w.add_error(30_000);
        w.advance(30_000);
        assert_eq!(w.error_count(), 2);
        w.advance(61_000);
        assert_eq!(w.error_count(), 1);
    }

    #[test]
    fn window_aggregations() {
        let mut w = Window::default();
        let mut r1 = record(0, Some(1.0));
        r1.prompt_tokens = 100;
        r1.prefix_cache_hit_tokens = 25;
        r1.drafted_tokens = 10;
        r1.accepted_tokens = 5;
        let mut r2 = record(1_000, Some(3.0));
        r2.prompt_tokens = 100;
        r2.prefix_cache_hit_tokens = 25;
        r2.drafted_tokens = 10;
        r2.accepted_tokens = 5;
        w.add_request(r1);
        w.add_request(r2);
        w.advance(1_000);
        assert_eq!(w.avg_ttft(), Some(2.0));
        assert_eq!(w.prefix_cache_hit_rate(), Some(50.0 / 200.0));
        assert_eq!(w.speculative_acceptance(), Some(10.0 / 20.0));
    }

    #[test]
    fn window_clear_resets() {
        let mut w = Window::default();
        w.add_request(record(0, Some(1.0)));
        w.add_error(0);
        w.advance(0);
        w.clear();
        assert!(w.anchor().is_none());
        assert_eq!(w.request_count(), 0);
        assert_eq!(w.error_count(), 0);
        assert_eq!(w.avg_ttft(), None);
    }

    #[test]
    fn ring_buffer_zero_capacity_holds_nothing() {
        let mut rb: RingBuffer<u32> = RingBuffer::new(0);
        for i in 0..10 {
            rb.push(i);
        }
        assert_eq!(rb.len(), 0);
        assert!(rb.is_empty());
        assert_eq!(rb.last(), None);
    }

    #[test]
    fn window_evicts_out_of_order_entries() {
        let mut w = Window::default();
        w.add_request(record(100_000, Some(1.0)));
        w.advance(100_000);
        // A stale record arrives out of order, after the anchor moved on.
        w.add_request(record(0, Some(9.0)));
        w.advance(100_000);
        // ts=0 is below the cutoff (40_000) and must be evicted even though
        // it sits at the back of the deque.
        assert_eq!(w.request_count(), 1);
        assert_eq!(w.avg_ttft(), Some(1.0));
    }

    #[test]
    fn window_range_and_cutoff() {
        let mut w = Window::default();
        assert!(w.cutoff().is_none());
        assert!(w.range().is_none());

        w.add_request(record(100, Some(1.0)));
        w.advance(100);
        assert_eq!(w.cutoff(), Some(0));
        assert_eq!(w.range(), Some((0, 100)));

        w.advance(70_000);
        assert_eq!(w.cutoff(), Some(10_000));
        assert_eq!(w.range(), Some((10_000, 70_000)));
    }

    #[test]
    fn window_previous_window_aggregates() {
        let mut w = Window::default();
        // Anchor 100_000: current window [40_000, 100_000], previous
        // window [0, 40_000).
        w.add_request(record(10_000, Some(1.0)));
        w.add_request(record(39_999, Some(3.0)));
        w.add_request(record(40_000, Some(7.0)));
        w.add_request(record(100_000, Some(9.0)));
        w.add_error(10_000);
        w.add_error(40_000);
        w.advance(100_000);
        assert_eq!(w.request_count(), 2);
        assert_eq!(w.avg_ttft(), Some(8.0));
        assert_eq!(w.error_count(), 1);
        assert_eq!(w.request_count_previous(), 2);
        assert_eq!(w.avg_ttft_previous(), Some(2.0));
        assert_eq!(w.error_count_previous(), 1);
    }

    #[test]
    fn window_boundary_belongs_to_current_window() {
        let mut w = Window::default();
        // Exactly anchor - WINDOW_MS is inside the current window, not the
        // previous one (the previous window's upper bound is exclusive).
        w.add_request(record(40_000, Some(1.0)));
        w.advance(100_000);
        assert_eq!(w.request_count(), 1);
        assert_eq!(w.request_count_previous(), 0);
        assert_eq!(w.avg_ttft_previous(), None);
    }

    #[test]
    fn window_previous_window_empty_without_anchor() {
        let w = Window::default();
        assert_eq!(w.request_count_previous(), 0);
        assert_eq!(w.error_count_previous(), 0);
        assert_eq!(w.avg_ttft_previous(), None);
        assert_eq!(w.avg_decode_latency_per_token_previous(), None);
        assert_eq!(w.prefix_cache_hit_rate_previous(), None);
        assert_eq!(w.speculative_acceptance_previous(), None);
    }

    #[test]
    fn window_previous_window_aggregates_rates() {
        let mut w = Window::default();
        let mut prev = record(10_000, Some(1.0));
        prev.prompt_tokens = 100;
        prev.prefix_cache_hit_tokens = 25;
        prev.drafted_tokens = 10;
        prev.accepted_tokens = 5;
        let mut cur = record(50_000, Some(2.0));
        cur.decode_latency_per_token = Some(0.25);
        cur.prompt_tokens = 100;
        cur.prefix_cache_hit_tokens = 50;
        cur.drafted_tokens = 20;
        cur.accepted_tokens = 4;
        w.add_request(prev);
        w.add_request(cur);
        w.advance(100_000);
        assert_eq!(w.prefix_cache_hit_rate_previous(), Some(25.0 / 100.0));
        assert_eq!(w.speculative_acceptance_previous(), Some(5.0 / 10.0));
        assert_eq!(w.avg_decode_latency_per_token_previous(), None);
        assert_eq!(w.prefix_cache_hit_rate(), Some(50.0 / 100.0));
        assert_eq!(w.speculative_acceptance(), Some(4.0 / 20.0));
        assert_eq!(w.avg_decode_latency_per_token(), Some(0.25));
    }

    #[test]
    fn window_retention_drops_entries_beyond_two_windows() {
        let mut w = Window::default();
        w.add_request(record(0, Some(1.0)));
        w.advance(0);
        w.advance(121_000);
        // ts=0 is below anchor - RETENTION_MS and physically evicted.
        assert_eq!(w.request_count(), 0);
        assert_eq!(w.request_count_previous(), 0);
        assert_eq!(w.avg_ttft_previous(), None);
    }
}
