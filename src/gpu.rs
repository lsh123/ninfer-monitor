// NInferMonitor
//
// This is free software; see the LICENSE.md file in the source distribution for precise wording.
//
// Copyright (C) 2026 Aleksey Sanin aleksey@aleksey.com. All Rights Reserved.

use std::os::raw::c_uint;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::chart::{self, ChartData, ChartKind, Series};
use crate::metrics::{MAX_SERIES_POINTS, RingBuffer};

/// The GPU temperature display unit (M4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TempUnit {
    Fahrenheit,
    Celsius,
}

impl TempUnit {
    pub const DEFAULT: TempUnit = TempUnit::Fahrenheit;

    /// Parse a stored unit string; `"F"` and `"C"` are recognized, anything
    /// else falls back to the default unit.
    pub fn parse(s: &str) -> TempUnit {
        match s {
            "F" => TempUnit::Fahrenheit,
            "C" => TempUnit::Celsius,
            _ => TempUnit::DEFAULT,
        }
    }

    /// The canonical stored form (`"F"` / `"C"`).
    pub fn as_str(self) -> &'static str {
        match self {
            TempUnit::Fahrenheit => "F",
            TempUnit::Celsius => "C",
        }
    }

    /// The display symbol for titles and axes.
    pub fn symbol(self) -> &'static str {
        match self {
            TempUnit::Fahrenheit => "°F",
            TempUnit::Celsius => "°C",
        }
    }
}

/// One GPU device's latest readings (M4). Every reading is `Option` so a
/// partially-failed poll (e.g. a device that fell off the bus) degrades
/// gracefully instead of dropping the whole snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct GpuDevice {
    pub index: u32,
    pub name: Option<String>,
    pub utilization: Option<u32>,
    pub memory_used: Option<u64>,
    pub memory_total: Option<u64>,
    /// Core temperature in degrees Celsius (NVML_TEMPERATURE_GPU).
    pub temperature: Option<u32>,
    /// VRAM/memory temperature in degrees Celsius (NVML_TEMPERATURE_MEMORY).
    pub memory_temperature: Option<u32>,
}

/// A poll of all GPUs (M4). `available` is false when NVML could not be
/// initialized (no NVIDIA GPU/driver, or an unsupported platform such as
/// macOS); the GPU section is then hidden.
#[derive(Debug, Clone, PartialEq)]
pub struct GpuSnapshot {
    pub available: bool,
    pub devices: Vec<GpuDevice>,
}

impl GpuSnapshot {
    pub fn unavailable() -> Self {
        Self {
            available: false,
            devices: Vec::new(),
        }
    }
}

/// One point in a GPU device's time series (M4). Temperatures are stored in
/// degrees Celsius (unit-agnostic); the display unit is applied at render
/// time so a unit change does not require re-polling.
#[derive(Debug, Clone, PartialEq)]
pub struct DevicePoint {
    /// The NVML device index this point belongs to (M4). History is keyed by
    /// this index rather than Vec position, so a device that drops out of a
    /// poll does not shift the other devices' series.
    pub index: u32,
    pub utilization: Option<f64>,
    pub memory_percent: Option<f64>,
    pub temperature: Option<f64>,
    pub memory_temperature: Option<f64>,
}

/// One poll of all GPUs, timestamped (M4).
#[derive(Debug, Clone, PartialEq)]
pub struct GpuSample {
    pub ts_ms: u64,
    pub devices: Vec<DevicePoint>,
}

/// Bounded history of GPU samples (M4), one `GpuSample` per drained poll.
#[derive(Debug, Clone)]
pub struct GpuHistory {
    buf: RingBuffer<GpuSample>,
}

impl Default for GpuHistory {
    fn default() -> Self {
        Self::new()
    }
}

impl GpuHistory {
    pub fn new() -> Self {
        Self {
            buf: RingBuffer::new(MAX_SERIES_POINTS),
        }
    }

    pub fn push(&mut self, sample: GpuSample) {
        self.buf.push(sample);
    }

    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &GpuSample> {
        self.buf.iter()
    }

    pub fn clear(&mut self) {
        self.buf.clear();
    }
}

/// Background NVML poller (M4). Polls all GPUs at the configured interval and
/// sends a `GpuSnapshot` over a channel for the UI to drain at ≤ 4 Hz.
pub struct GpuPoller {
    rx: Mutex<mpsc::Receiver<GpuSnapshot>>,
    stop: Arc<AtomicBool>,
    interval_ms: Arc<AtomicU64>,
    handle: Option<JoinHandle<()>>,
}

impl GpuPoller {
    /// Start the poll thread, polling every `interval_ms`.
    pub fn start(interval_ms: u64) -> Self {
        let (tx, rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let interval_ms = Arc::new(AtomicU64::new(interval_ms));
        let handle = thread::Builder::new()
            .name("gpu-poll".to_string())
            .spawn({
                let tx = tx;
                let stop = stop.clone();
                let interval_ms = interval_ms.clone();
                move || gpu_poll_loop(tx, stop, interval_ms)
            })
            .expect("failed to spawn gpu poll thread");
        Self {
            rx: Mutex::new(rx),
            stop,
            interval_ms,
            handle: Some(handle),
        }
    }

    /// The current poll interval in milliseconds (M4).
    pub fn poll_interval_ms(&self) -> u64 {
        self.interval_ms.load(Ordering::Relaxed)
    }

    /// Change the poll interval (M4: it follows the settings GPU poll
    /// interval).
    pub fn set_poll_interval(&self, ms: u64) {
        self.interval_ms.store(ms.max(1), Ordering::Relaxed);
    }

    /// Take the newest pending snapshot without blocking, dropping older ones.
    pub fn take_latest(&self) -> Option<GpuSnapshot> {
        let rx = self.rx.lock().unwrap();
        let mut latest = None;
        while let Ok(snap) = rx.try_recv() {
            latest = Some(snap);
        }
        latest
    }

    /// Signal the poll thread to stop and wait for it to exit.
    pub fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for GpuPoller {
    /// A dropped poller stops and joins its thread so a replaced or exiting
    /// app does not leave a detached thread polling NVML.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Poll thread body (M4): initialize NVML once, then poll all devices at the
/// configured interval, sending a `GpuSnapshot` each time. When NVML is
/// unavailable, report it once and idle until stopped.
fn gpu_poll_loop(
    tx: mpsc::Sender<GpuSnapshot>,
    stop: Arc<AtomicBool>,
    interval_ms: Arc<AtomicU64>,
) {
    #[cfg(any(windows, target_os = "linux"))]
    {
        let Ok(nvml) = nvml_wrapper::Nvml::init() else {
            let _ = tx.send(GpuSnapshot::unavailable());
            while !stop.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(500));
            }
            return;
        };
        let lib = nvml.lib();
        let mut last_devices: Option<Vec<GpuDevice>> = None;
        loop {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            let snap = poll_devices(&nvml, lib, last_devices.as_deref());
            if snap.available {
                last_devices = Some(snap.devices.clone());
            }
            if tx.send(snap).is_err() {
                break;
            }
            sleep_interruptible(interval_ms.load(Ordering::Relaxed).max(1), &stop);
        }
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = interval_ms;
        let _ = tx.send(GpuSnapshot::unavailable());
        while !stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(500));
        }
    }
}

/// Sleep for `ms`, waking early if `stop` is set (so shutdown is prompt).
fn sleep_interruptible(ms: u64, stop: &AtomicBool) {
    let deadline = Instant::now() + Duration::from_millis(ms);
    while !stop.load(Ordering::Relaxed) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Poll every GPU once (M4), reading the name, utilization, memory, the core
/// temperature, and the VRAM/memory temperature. When `device_count()` fails,
/// a transient driver error (e.g. a timeout under load) degrades to
/// `last_devices` — the previous poll's device list — so a single bad poll
/// does not hide the whole GPU section; with no previous list the snapshot is
/// unavailable.
/// The snapshot to send when `device_count()` fails (M4): the previous
/// poll's device list when there is one, otherwise unavailable.
fn count_error_snapshot(last_devices: Option<&[GpuDevice]>) -> GpuSnapshot {
    match last_devices {
        Some(devices) => GpuSnapshot {
            available: true,
            devices: devices.to_vec(),
        },
        None => GpuSnapshot::unavailable(),
    }
}

#[cfg(any(windows, target_os = "linux"))]
fn poll_devices(
    nvml: &nvml_wrapper::Nvml,
    lib: &nvml_wrapper_sys::bindings::NvmlLib,
    last_devices: Option<&[GpuDevice]>,
) -> GpuSnapshot {
    let Ok(count) = nvml.device_count() else {
        return count_error_snapshot(last_devices);
    };
    let mut devices = Vec::with_capacity(count as usize);
    for i in 0..count {
        let Ok(device) = nvml.device_by_index(i) else {
            continue;
        };
        let name = device.name().ok();
        let utilization = device.utilization_rates().ok().map(|u| u.gpu);
        let (memory_used, memory_total) = match device.memory_info().ok() {
            Some(m) => (Some(m.total - m.free), Some(m.total)),
            None => (None, None),
        };
        let temperature = device
            .temperature(nvml_wrapper::enum_wrappers::device::TemperatureSensor::Gpu)
            .ok();
        let memory_temperature = gpu_memory_temperature(lib, i);
        devices.push(GpuDevice {
            index: i,
            name,
            utilization,
            memory_used,
            memory_total,
            temperature,
            memory_temperature,
        });
    }
    GpuSnapshot {
        available: true,
        devices,
    }
}

/// Read the live `NVML_TEMPERATURE_MEMORY` (VRAM sensor, value 2) for device
/// `index` (M4).
///
/// The safe `nvml-wrapper` API does not expose the memory-temperature sensor
/// (its `TemperatureSensor` enum wraps only the GPU core sensor), so this is
/// an isolated `unsafe` FFI block: it reuses the already-loaded NVML library
/// (via `Nvml::lib`) and calls `nvmlDeviceGetTemperature` directly. NVML is
/// assumed already initialized by the `nvml_wrapper::Nvml` handle. A driver
/// that does not report the sensor (or a device that is inaccessible) yields
/// `None`.
#[cfg(any(windows, target_os = "linux"))]
fn gpu_memory_temperature(lib: &nvml_wrapper_sys::bindings::NvmlLib, index: u32) -> Option<u32> {
    use nvml_wrapper_sys::bindings::{nvmlDevice_t, nvmlReturn_t};
    const NVML_SUCCESS: nvmlReturn_t = 0;
    const NVML_TEMPERATURE_MEMORY: c_uint = 2;
    unsafe {
        let mut handle: nvmlDevice_t = std::ptr::null_mut();
        if lib.nvmlDeviceGetHandleByIndex_v2(index as c_uint, &mut handle) != NVML_SUCCESS {
            return None;
        }
        let mut temp: c_uint = 0;
        if lib.nvmlDeviceGetTemperature(handle, NVML_TEMPERATURE_MEMORY, &mut temp) != NVML_SUCCESS
        {
            return None;
        }
        Some(temp)
    }
}

/// Convert a Celsius temperature to the display unit (M4).
pub fn celsius_to_display(celsius: u32, unit: TempUnit) -> f64 {
    match unit {
        TempUnit::Celsius => celsius as f64,
        TempUnit::Fahrenheit => celsius as f64 * 9.0 / 5.0 + 32.0,
    }
}

/// Format a GPU utilization percentage (M4).
pub fn format_utilization(util: Option<u32>) -> String {
    match util {
        Some(u) => u.to_string(),
        None => crate::kpi::NO_DATA.to_owned(),
    }
}

/// Format a byte count as GB (M4).
pub fn format_bytes(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0 * 1024.0)
}

/// Format GPU memory as "0.5GB\n95%" (M4): the absolute used value on the
/// first line and the percentage of total on the second.
pub fn format_memory(used: Option<u64>, total: Option<u64>) -> String {
    let (Some(used), Some(total)) = (used, total) else {
        return crate::kpi::NO_DATA.to_owned();
    };
    if total == 0 {
        return format!("{:.1}GB", format_bytes(used));
    }
    let pct = used as f64 / total as f64 * 100.0;
    format!("{:.1}GB\n{:.0}%", format_bytes(used), pct)
}

/// The memory usage percentage (M4), for the chart's right axis.
pub fn memory_percent(used: Option<u64>, total: Option<u64>) -> Option<f64> {
    let (used, total) = (used?, total?);
    if total == 0 {
        return None;
    }
    Some(used as f64 / total as f64 * 100.0)
}

/// Format a temperature for display (M4), in the active unit, rounded to a
/// whole degree.
pub fn format_temperature(celsius: Option<u32>, unit: TempUnit) -> String {
    match celsius {
        Some(c) => format!("{}", celsius_to_display(c, unit).round() as u32),
        None => crate::kpi::NO_DATA.to_owned(),
    }
}

/// Format the VRAM temperature for display (M4), in the active unit, or
/// `"NA"` when the driver does not report the memory-temperature sensor.
pub fn format_vram_temperature(celsius: Option<u32>, unit: TempUnit) -> String {
    match celsius {
        Some(c) => format_temperature(Some(c), unit),
        None => "NA".to_owned(),
    }
}

/// Convert a `GpuSnapshot` into a `GpuSample` for the history (M4).
pub fn sample_from_snapshot(snap: &GpuSnapshot, ts_ms: u64) -> GpuSample {
    let devices = snap
        .devices
        .iter()
        .map(|d| DevicePoint {
            index: d.index,
            utilization: d.utilization.map(|u| u as f64),
            memory_percent: memory_percent(d.memory_used, d.memory_total),
            temperature: d.temperature.map(|t| t as f64),
            memory_temperature: d.memory_temperature.map(|t| t as f64),
        })
        .collect();
    GpuSample { ts_ms, devices }
}

fn empty_chart(kind: ChartKind) -> ChartData {
    ChartData {
        kind,
        domain: (0.0, 1.0),
        series: Vec::new(),
        restarts: Vec::new(),
    }
}

fn f64_cmp(a: &(f64, f64), b: &(f64, f64)) -> std::cmp::Ordering {
    a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal)
}

/// The newest sample's timestamp, if any (M4).
fn history_end(history: &GpuHistory) -> Option<u64> {
    history.iter().last().map(|s| s.ts_ms)
}

/// Prepare the GPU usage/memory chart data for the device with NVML index
/// `index` (M4): left axis = utilization %, right axis = memory %.
/// The domain's right edge is the current time (`now_ms`, M5) so a stale
/// series gets a dashed tail to the right edge.
pub fn prepare_usage(history: &GpuHistory, index: u32, window_ms: u64, now_ms: u64) -> ChartData {
    let Some(t_end) = history_end(history) else {
        return empty_chart(ChartKind::GpuUsage);
    };
    let t_end = t_end as f64;
    let t_start = (t_end - window_ms as f64).max(0.0);
    let t1 = t_end.max(now_ms as f64);
    let mut utilization = Vec::new();
    let mut memory = Vec::new();
    for s in history.iter() {
        let ts = s.ts_ms as f64;
        if ts < t_start || ts > t1 {
            continue;
        }
        let Some(point) = s.devices.iter().find(|p| p.index == index) else {
            continue;
        };
        if let Some(v) = point.utilization {
            utilization.push((ts, v));
        }
        if let Some(v) = point.memory_percent {
            memory.push((ts, v));
        }
    }
    utilization.sort_by(f64_cmp);
    memory.sort_by(f64_cmp);
    ChartData {
        kind: ChartKind::GpuUsage,
        domain: (t_start, t1),
        series: vec![
            Series {
                name: "usage",
                unit: "%",
                points: utilization,
            },
            Series {
                name: "memory",
                unit: "%",
                points: memory,
            },
        ],
        restarts: Vec::new(),
    }
}

/// Prepare the GPU temperature chart data for the device with NVML index
/// `index` (M4): left axis = Core, right axis = VRAM, both in the display
/// unit. The domain's right edge is the current time (`now_ms`, M5).
pub fn prepare_temp(
    history: &GpuHistory,
    index: u32,
    window_ms: u64,
    unit: TempUnit,
    now_ms: u64,
) -> ChartData {
    let Some(t_end) = history_end(history) else {
        return empty_chart(ChartKind::GpuTemp);
    };
    let t_end = t_end as f64;
    let t_start = (t_end - window_ms as f64).max(0.0);
    let t1 = t_end.max(now_ms as f64);
    let mut vram = Vec::new();
    let mut core = Vec::new();
    for s in history.iter() {
        let ts = s.ts_ms as f64;
        if ts < t_start || ts > t1 {
            continue;
        }
        let Some(point) = s.devices.iter().find(|p| p.index == index) else {
            continue;
        };
        // Temperatures are stored in Celsius; convert to the display unit so
        // the chart (and its axis) are in the active unit.
        if let Some(v) = point.memory_temperature {
            vram.push((ts, celsius_to_display(v as u32, unit)));
        }
        if let Some(v) = point.temperature {
            core.push((ts, celsius_to_display(v as u32, unit)));
        }
    }
    vram.sort_by(f64_cmp);
    core.sort_by(f64_cmp);
    ChartData {
        kind: ChartKind::GpuTemp,
        domain: (t_start, t1),
        series: vec![
            Series {
                name: "core",
                unit: unit.symbol(),
                points: core,
            },
            Series {
                name: "vram",
                unit: unit.symbol(),
                points: vram,
            },
        ],
        restarts: Vec::new(),
    }
}

/// The display data for one GPU row (M4), ready to map to the slint
/// `GpuDevice` model. Kept in plain Rust so `main.rs` and the tests share the
/// row-building logic.
#[derive(Debug, Clone)]
pub struct GpuRowData {
    pub usage_title: String,
    pub usage_left_label: String,
    pub usage_left_value: String,
    pub usage_left_unit: String,
    pub usage_right_label: String,
    pub usage_right_value: String,
    pub usage_image: slint::Image,
    pub temp_title: String,
    pub temp_left_label: String,
    pub temp_left_value: String,
    pub temp_left_unit: String,
    pub temp_right_label: String,
    pub temp_right_value: String,
    pub temp_right_unit: String,
    pub temp_image: slint::Image,
}

/// Build the GPU rows (M4) from the latest snapshot and history. `size` is the
/// render size of each GPU chart (the metric chart size); `now_ms` is the
/// current time, the charts' right edge (M5). Returns an empty list when NVML
/// is unavailable.
pub fn build_rows(
    snapshot: &GpuSnapshot,
    history: &GpuHistory,
    unit: TempUnit,
    window_ms: u64,
    size: (u32, u32),
    now_ms: u64,
) -> Vec<GpuRowData> {
    if !snapshot.available {
        return Vec::new();
    }
    snapshot
        .devices
        .iter()
        .map(|d| {
            let label = format!("GPU{}", d.index);
            let name = d.name.clone().unwrap_or_else(|| label.clone());
            let usage_title = format!("{label} ({name}): usage / memory");
            let temp_title = format!("{label} temperature");
            let usage = prepare_usage(history, d.index, window_ms, now_ms);
            let temp = prepare_temp(history, d.index, window_ms, unit, now_ms);
            let usage_image = chart::to_image(&chart::render(&usage, size), size);
            let temp_image = chart::to_image(&chart::render(&temp, size), size);
            let temp_symbol = unit.symbol().to_owned();
            GpuRowData {
                usage_title,
                usage_left_label: format!("{label} Usage"),
                usage_left_value: format_utilization(d.utilization),
                usage_left_unit: if d.utilization.is_some() {
                    "%".to_owned()
                } else {
                    String::new()
                },
                usage_right_label: format!("{label} Memory"),
                usage_right_value: format_memory(d.memory_used, d.memory_total),
                usage_image,
                temp_title,
                temp_left_label: "Core".to_owned(),
                temp_left_value: format_temperature(d.temperature, unit),
                temp_left_unit: if d.temperature.is_some() {
                    temp_symbol.clone()
                } else {
                    String::new()
                },
                temp_right_label: "VRAM".to_owned(),
                temp_right_value: format_vram_temperature(d.memory_temperature, unit),
                temp_right_unit: if d.memory_temperature.is_some() {
                    temp_symbol
                } else {
                    String::new()
                },
                temp_image,
            }
        })
        .collect()
}

/// The current wall-clock time in unix milliseconds (M4).
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(index: u32, util: u32, used: u64, total: u64, temp: u32, mem_temp: u32) -> GpuDevice {
        GpuDevice {
            index,
            name: Some(format!("NVIDIA GPU {index}")),
            utilization: Some(util),
            memory_used: Some(used),
            memory_total: Some(total),
            temperature: Some(temp),
            memory_temperature: Some(mem_temp),
        }
    }

    fn snapshot(devices: Vec<GpuDevice>) -> GpuSnapshot {
        GpuSnapshot {
            available: true,
            devices,
        }
    }

    #[test]
    fn count_error_snapshot_keeps_last_devices() {
        let devices = vec![device(0, 10, 512, 1024, 60, 70)];
        let snap = count_error_snapshot(Some(&devices));
        assert!(snap.available);
        assert_eq!(snap.devices, devices);
        let snap = count_error_snapshot(None);
        assert!(!snap.available);
        assert!(snap.devices.is_empty());
    }

    #[test]
    fn temp_unit_round_trips() {
        assert_eq!(TempUnit::parse("F"), TempUnit::Fahrenheit);
        assert_eq!(TempUnit::parse("C"), TempUnit::Celsius);
        assert_eq!(TempUnit::parse("c"), TempUnit::DEFAULT);
        assert_eq!(TempUnit::parse(""), TempUnit::DEFAULT);
        assert_eq!(TempUnit::Fahrenheit.as_str(), "F");
        assert_eq!(TempUnit::Celsius.as_str(), "C");
        assert_eq!(TempUnit::Fahrenheit.symbol(), "°F");
        assert_eq!(TempUnit::Celsius.symbol(), "°C");
    }

    #[test]
    fn celsius_to_display_converts() {
        assert_eq!(celsius_to_display(100, TempUnit::Celsius), 100.0);
        assert_eq!(celsius_to_display(100, TempUnit::Fahrenheit), 212.0);
        assert_eq!(celsius_to_display(0, TempUnit::Fahrenheit), 32.0);
    }

    #[test]
    fn format_utilization_shows_percent_or_no_data() {
        assert_eq!(format_utilization(Some(100)), "100");
        assert_eq!(format_utilization(Some(0)), "0");
        assert_eq!(format_utilization(None), crate::kpi::NO_DATA);
    }

    #[test]
    fn format_memory_shows_absolute_and_percent() {
        let used = 512 * 1024 * 1024;
        let total = 512 * 1024 * 1024;
        assert_eq!(format_memory(Some(used), Some(total)), "0.5GB\n100%");
        let total = 4 * 1024 * 1024 * 1024;
        assert_eq!(format_memory(Some(used), Some(total)), "0.5GB\n12%");
        assert_eq!(format_memory(None, Some(total)), crate::kpi::NO_DATA);
        assert_eq!(format_memory(Some(used), None), crate::kpi::NO_DATA);
    }

    #[test]
    fn format_memory_zero_total_shows_absolute_only() {
        let used = 256 * 1024 * 1024;
        assert_eq!(format_memory(Some(used), Some(0)), "0.2GB");
    }

    #[test]
    fn memory_percent_computes() {
        assert_eq!(memory_percent(Some(512), Some(1024)), Some(50.0));
        assert_eq!(memory_percent(Some(1), Some(0)), None);
        assert_eq!(memory_percent(None, Some(10)), None);
    }

    #[test]
    fn format_temperature_uses_unit() {
        assert_eq!(format_temperature(Some(100), TempUnit::Celsius), "100");
        assert_eq!(format_temperature(Some(100), TempUnit::Fahrenheit), "212");
        assert_eq!(
            format_temperature(None, TempUnit::Fahrenheit),
            crate::kpi::NO_DATA
        );
    }

    #[test]
    fn format_vram_temperature_shows_na_when_unavailable() {
        assert_eq!(
            format_vram_temperature(Some(70), TempUnit::Fahrenheit),
            "158"
        );
        assert_eq!(format_vram_temperature(Some(70), TempUnit::Celsius), "70");
        assert_eq!(format_vram_temperature(None, TempUnit::Fahrenheit), "NA");
        assert_eq!(format_vram_temperature(None, TempUnit::Celsius), "NA");
    }

    #[test]
    fn sample_from_snapshot_maps_fields() {
        let snap = snapshot(vec![device(0, 80, 512, 1024, 60, 70)]);
        let sample = sample_from_snapshot(&snap, 1_000);
        assert_eq!(sample.ts_ms, 1_000);
        assert_eq!(sample.devices.len(), 1);
        let p = &sample.devices[0];
        assert_eq!(p.index, 0);
        assert_eq!(p.utilization, Some(80.0));
        assert_eq!(p.memory_percent, Some(50.0));
        assert_eq!(p.temperature, Some(60.0));
        assert_eq!(p.memory_temperature, Some(70.0));
    }

    #[test]
    fn history_is_bounded_fifo() {
        let mut h = GpuHistory::new();
        for i in 0..(MAX_SERIES_POINTS + 5) {
            h.push(GpuSample {
                ts_ms: i as u64,
                devices: Vec::new(),
            });
        }
        assert_eq!(h.len(), MAX_SERIES_POINTS);
        assert_eq!(h.iter().next().unwrap().ts_ms, 5);
    }

    #[test]
    fn prepare_usage_empty_history_has_no_points() {
        let h = GpuHistory::new();
        let data = prepare_usage(&h, 0, 300_000, 0);
        assert!(data.series.iter().all(|s| s.points.is_empty()));
    }

    #[test]
    fn prepare_usage_collects_window_points() {
        let mut h = GpuHistory::new();
        for ts in [0, 100_000, 200_000] {
            let snap = snapshot(vec![device(0, 10, 512, 1024, 60, 70)]);
            h.push(sample_from_snapshot(&snap, ts));
        }
        let data = prepare_usage(&h, 0, 150_000, 200_000);
        assert_eq!(data.series.len(), 2);
        assert_eq!(data.series[0].name, "usage");
        assert_eq!(data.series[0].points.len(), 2);
        assert_eq!(data.series[1].name, "memory");
        assert_eq!(
            data.series[1].points,
            vec![(100_000.0, 50.0), (200_000.0, 50.0)]
        );
    }

    #[test]
    fn prepare_temp_converts_to_display_unit() {
        let mut h = GpuHistory::new();
        let snap = snapshot(vec![device(0, 10, 512, 1024, 60, 70)]);
        h.push(sample_from_snapshot(&snap, 1_000));
        let data = prepare_temp(&h, 0, 300_000, TempUnit::Fahrenheit, 1_000);
        assert_eq!(data.series[0].name, "core");
        // 60 C -> 140 F.
        assert_eq!(data.series[0].points, vec![(1_000.0, 140.0)]);
        assert_eq!(data.series[1].name, "vram");
        // 70 C -> 158 F.
        assert_eq!(data.series[1].points, vec![(1_000.0, 158.0)]);
    }

    #[test]
    fn build_rows_empty_when_unavailable() {
        let h = GpuHistory::new();
        let rows = build_rows(
            &GpuSnapshot::unavailable(),
            &h,
            TempUnit::Fahrenheit,
            300_000,
            (300, 100),
            1_000,
        );
        assert!(rows.is_empty());
    }

    #[test]
    fn build_rows_formats_labels_and_values() {
        let mut h = GpuHistory::new();
        let snap = snapshot(vec![device(
            0,
            80,
            512 * 1024 * 1024,
            4 * 1024 * 1024 * 1024,
            60,
            70,
        )]);
        h.push(sample_from_snapshot(&snap, 1_000));
        let rows = build_rows(&snap, &h, TempUnit::Fahrenheit, 300_000, (300, 100), 1_000);
        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert_eq!(r.usage_title, "GPU0 (NVIDIA GPU 0): usage / memory");
        assert_eq!(r.usage_left_label, "GPU0 Usage");
        assert_eq!(r.usage_left_value, "80");
        assert_eq!(r.usage_left_unit, "%");
        assert_eq!(r.usage_right_label, "GPU0 Memory");
        assert_eq!(r.usage_right_value, "0.5GB\n12%");
        assert_eq!(r.temp_title, "GPU0 temperature");
        assert_eq!(r.temp_left_label, "Core");
        assert_eq!(r.temp_left_value, "140");
        assert_eq!(r.temp_left_unit, "°F");
        assert_eq!(r.temp_right_label, "VRAM");
        assert_eq!(r.temp_right_value, "158");
        assert_eq!(r.temp_right_unit, "°F");
    }

    #[test]
    fn build_rows_vram_na_when_sensor_unavailable() {
        let mut h = GpuHistory::new();
        let snap = snapshot(vec![GpuDevice {
            index: 0,
            name: Some("NVIDIA GPU 0".to_owned()),
            utilization: Some(80),
            memory_used: Some(512 * 1024 * 1024),
            memory_total: Some(4 * 1024 * 1024 * 1024),
            temperature: Some(60),
            memory_temperature: None,
        }]);
        h.push(sample_from_snapshot(&snap, 1_000));
        let rows = build_rows(&snap, &h, TempUnit::Fahrenheit, 300_000, (300, 100), 1_000);
        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert_eq!(r.temp_left_label, "Core");
        assert_eq!(r.temp_left_value, "140");
        assert_eq!(r.temp_left_unit, "°F");
        assert_eq!(r.temp_right_label, "VRAM");
        assert_eq!(r.temp_right_value, "NA");
        assert_eq!(r.temp_right_unit, "");
    }

    #[test]
    fn prepare_usage_domain_extends_to_now() {
        let mut h = GpuHistory::new();
        h.push(sample_from_snapshot(
            &snapshot(vec![device(0, 10, 512, 1024, 60, 70)]),
            100_000,
        ));
        // Stale: the domain's right edge is the current time, past the last
        // sample (M5).
        let data = prepare_usage(&h, 0, 300_000, 160_000);
        assert_eq!(data.domain, (0.0, 160_000.0));
        // Clock skew: `now` earlier than the last sample keeps the last
        // sample at the right edge.
        let data = prepare_usage(&h, 0, 300_000, 50_000);
        assert_eq!(data.domain, (0.0, 100_000.0));
    }

    #[test]
    fn prepare_temp_domain_extends_to_now() {
        let mut h = GpuHistory::new();
        h.push(sample_from_snapshot(
            &snapshot(vec![device(0, 10, 512, 1024, 60, 70)]),
            100_000,
        ));
        let data = prepare_temp(&h, 0, 300_000, TempUnit::Fahrenheit, 160_000);
        assert_eq!(data.domain, (0.0, 160_000.0));
    }

    #[test]
    fn prepare_usage_keys_history_by_device_index() {
        let mut h = GpuHistory::new();
        h.push(sample_from_snapshot(
            &snapshot(vec![
                device(0, 10, 512, 1024, 60, 70),
                device(1, 20, 512, 1024, 61, 71),
            ]),
            0,
        ));
        // Device 1 drops out of the poll; device 2 takes its Vec position.
        h.push(sample_from_snapshot(
            &snapshot(vec![
                device(0, 11, 512, 1024, 60, 70),
                device(2, 99, 512, 1024, 62, 72),
            ]),
            100_000,
        ));
        // Device 1's series must contain only its own point, not device 2's
        // shifted-in value.
        let data = prepare_usage(&h, 1, 150_000, 100_000);
        assert_eq!(data.series[0].points, vec![(0.0, 20.0)]);
        // Device 2's series must contain only its own point.
        let data = prepare_usage(&h, 2, 150_000, 100_000);
        assert_eq!(data.series[0].points, vec![(100_000.0, 99.0)]);
    }
}
