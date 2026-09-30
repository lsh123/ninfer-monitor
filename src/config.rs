// NInferMonitor
//
// This is free software; see the LICENSE.md file in the source distribution for precise wording.
//
// Copyright (C) 2026 Aleksey Sanin aleksey@aleksey.com. All Rights Reserved.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::columns::{TableColumns, parse_table_columns};
use crate::gpu::TempUnit;
use crate::split::{SplitPanels, parse_split_panels};
use crate::update_check::clamp_period_days;
use crate::window_state::{WindowState, parse_window_state};

use crate::tail::DEFAULT_LOG_POLL_INTERVAL;

pub const MIN_LOG_POLL_MS: u64 = 100;
pub const MAX_LOG_POLL_MS: u64 = 5_000;
pub const MIN_GPU_POLL_MS: u64 = 1_000;
pub const MAX_GPU_POLL_MS: u64 = 10_000;
pub const DEFAULT_GPU_POLL_MS: u64 = 5_000;
pub const MIN_WINDOW_MS: u64 = 60_000;
pub const MAX_WINDOW_MS: u64 = 3_600_000;
pub const DEFAULT_WINDOW_MS: u64 = 300_000;
pub const MIN_MAX_REQUESTS: u64 = 10;
pub const MAX_MAX_REQUESTS: u64 = 1_000;
pub const DEFAULT_MAX_REQUESTS: u64 = 1_000;
/// The Control-tab split: the default share of the middle area given to the
/// configurations list (the edit panel takes the rest).
pub const DEFAULT_CONTROL_SPLIT: f32 = 0.5;
/// The Control-tab split: the configurations list may take at most this share
/// of the middle area, leaving at least `1 - MAX_CONTROL_SPLIT` for the edit
/// panel.
pub const MAX_CONTROL_SPLIT: f32 = 0.7;
/// The Control-tab split: the configurations list may take at least this
/// share of the middle area.
pub const MIN_CONTROL_SPLIT: f32 = 0.15;

pub const CONFIG_DIR: &str = ".ninfer-monitor";
pub const CONFIG_FILE: &str = "config.json";

/// A single NInfer configuration (v0.3 M3): a name and the command line
/// options (an opaque string the app does not parse, except to scan for
/// `--request-log-jsonl`, §4.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigEntry {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub command_line: String,
}

/// Persisted settings (FR-7.2): NInfer installation folder, log file
/// folder, log file poll interval, GPU poll interval, chart window, max
/// request rows, temperature unit, split-panel fractions, request-table
/// column widths, window geometry, and the automatic update-check state
/// (M8).
///
/// Unknown fields are ignored (forward compatibility); missing fields fall
/// back to the defaults below.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Config {
    /// The NInfer installation folder, if any (v0.3 M2).
    pub install_folder: Option<String>,
    /// The base folder for the app-managed log file (v0.3 M2); the monitored
    /// file is `<log_file_folder>/ninfer-monitor/server.requests.jsonl`.
    pub log_file_folder: String,
    pub log_poll_interval_ms: u64,
    pub gpu_poll_interval_ms: u64,
    pub chart_window_ms: u64,
    pub max_requests: u64,
    /// GPU temperature display unit, stored as `"F"` or `"C"` (M4).
    pub temp_unit: String,
    pub split_panels: SplitPanels,
    /// The Control-tab split: the share of the middle area given to the
    /// configurations list (the edit panel takes the rest).
    pub control_split: f32,
    /// The request-table column widths and the window width they were saved
    /// at (M7); restored only when the window width is unchanged.
    pub table_columns: Option<TableColumns>,
    pub window_state: Option<WindowState>,
    /// Whether automatic update checks are enabled (M8).
    pub update_check_enabled: bool,
    /// The update-check period in days, clamped to 1..=14 (M8).
    pub update_check_period_days: u64,
    /// The time of the last successful update check, in unix milliseconds
    /// (M8); `None` when a check has never succeeded.
    pub last_update_check_unix_ms: Option<u64>,
    /// The registered NInfer configurations (v0.3 M3).
    pub configs: Vec<ConfigEntry>,
    pub running_config: Option<String>,
}

/// The default log file folder: the OS temp folder (v0.3 M2).
pub fn default_log_file_folder() -> String {
    std::env::temp_dir().to_string_lossy().to_string()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            install_folder: None,
            log_file_folder: default_log_file_folder(),
            log_poll_interval_ms: DEFAULT_LOG_POLL_INTERVAL.as_millis() as u64,
            gpu_poll_interval_ms: DEFAULT_GPU_POLL_MS,
            chart_window_ms: DEFAULT_WINDOW_MS,
            max_requests: DEFAULT_MAX_REQUESTS,
            temp_unit: TempUnit::DEFAULT.as_str().to_owned(),
            split_panels: SplitPanels::default(),
            control_split: DEFAULT_CONTROL_SPLIT,
            table_columns: None,
            window_state: None,
            update_check_enabled: true,
            update_check_period_days: crate::update_check::DEFAULT_PERIOD_DAYS,
            last_update_check_unix_ms: None,
            configs: Vec::new(),
            running_config: None,
        }
    }
}

impl Config {
    /// Log file poll interval clamped to the supported range (FR-1.2).
    pub fn log_poll_interval(&self) -> Duration {
        Duration::from_millis(
            self.log_poll_interval_ms
                .clamp(MIN_LOG_POLL_MS, MAX_LOG_POLL_MS),
        )
    }

    /// GPU poll interval clamped to the supported range (M4).
    pub fn gpu_poll_interval(&self) -> Duration {
        Duration::from_millis(
            self.gpu_poll_interval_ms
                .clamp(MIN_GPU_POLL_MS, MAX_GPU_POLL_MS),
        )
    }

    /// Chart window clamped to the supported range (FR-4.2).
    pub fn chart_window_ms(&self) -> u64 {
        self.chart_window_ms.clamp(MIN_WINDOW_MS, MAX_WINDOW_MS)
    }

    /// Max request rows clamped to the supported range (FR-5.5).
    pub fn max_requests(&self) -> u64 {
        self.max_requests.clamp(MIN_MAX_REQUESTS, MAX_MAX_REQUESTS)
    }

    /// The GPU temperature display unit (M4); anything other than `"C"` is
    /// Fahrenheit.
    pub fn temp_unit(&self) -> TempUnit {
        TempUnit::parse(&self.temp_unit)
    }

    /// The update-check period clamped to the supported range (M8).
    pub fn update_check_period_days(&self) -> u64 {
        clamp_period_days(self.update_check_period_days)
    }

    /// The Control-tab split fraction clamped to the supported range.
    pub fn control_split(&self) -> f32 {
        self.control_split
            .clamp(MIN_CONTROL_SPLIT, MAX_CONTROL_SPLIT)
    }
}

/// Default config location: `$HOME/.ninfer-monitor/config.json` (FR-7.2),
/// where `$HOME` is `%USERPROFILE%` on Windows. `None` when the home
/// directory cannot be determined (persistence is then skipped).
pub fn default_config_path() -> Option<PathBuf> {
    home_dir().map(|home| home.join(CONFIG_DIR).join(CONFIG_FILE))
}

fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        if let Some(home) = std::env::var_os("USERPROFILE").filter(|v| !v.is_empty()) {
            return Some(PathBuf::from(home));
        }
        let home_path = std::env::var_os("HOMEPATH").filter(|v| !v.is_empty())?;
        let system_drive = std::env::var_os("SystemDrive").unwrap_or_else(|| "C:".into());
        let home = format!(
            "{}{}",
            system_drive.to_string_lossy(),
            home_path.to_string_lossy()
        );
        Some(PathBuf::from(home))
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    }
}

/// The on-disk state of a config file, as seen by a single read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigFileState {
    /// The file does not exist.
    Missing,
    /// The file is a valid JSON object.
    Valid,
    /// The file exists but is unreadable or not a JSON object.
    Malformed,
}

/// Read and parse the config at `path` in a single pass, reporting the
/// on-disk state so the caller can decide whether to repair the file
/// without re-reading it. A missing or malformed file yields the defaults
/// so a bad config can never break startup (NFR-4). Fields are parsed
/// independently: a missing or wrong-typed field keeps its default without
/// discarding the other fields.
pub fn load_with_state(path: &Path) -> (Config, ConfigFileState) {
    let data = match std::fs::read_to_string(path) {
        Ok(data) => data,
        // A missing file is not a problem; any other read error (non-UTF-8
        // bytes, permissions) means the file exists but is unusable, so it is
        // reported as `Malformed` and repaired on the next save.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return (Config::default(), ConfigFileState::Missing);
        }
        Err(_) => return (Config::default(), ConfigFileState::Malformed),
    };
    let Some(object) = serde_json::from_str::<serde_json::Value>(&data)
        .ok()
        .and_then(|value| value.as_object().cloned())
    else {
        return (Config::default(), ConfigFileState::Malformed);
    };
    let mut config = Config::default();
    if let Some(value) = object.get("install_folder").and_then(|v| v.as_str()) {
        if !value.trim().is_empty() {
            config.install_folder = Some(value.to_owned());
        }
    } else if let Some(value) = object.get("last_path").and_then(|v| v.as_str()) {
        // One-time migration (v0.3 M2): a v0.2 `last_path` pointing at
        // `<install>/logs/server.requests.jsonl` implies the install folder;
        // any other path is not an install folder and is dropped.
        config.install_folder = legacy_install_folder(value);
    }
    if let Some(value) = object.get("log_file_folder").and_then(|v| v.as_str()) {
        config.log_file_folder = if value.trim().is_empty() {
            default_log_file_folder()
        } else {
            value.to_owned()
        };
    }
    if let Some(value) = object.get("log_poll_interval_ms").and_then(|v| v.as_u64()) {
        config.log_poll_interval_ms = value;
    }
    if let Some(value) = object.get("gpu_poll_interval_ms").and_then(|v| v.as_u64()) {
        config.gpu_poll_interval_ms = value;
    }
    if let Some(value) = object.get("chart_window_ms").and_then(|v| v.as_u64()) {
        config.chart_window_ms = value;
    }
    if let Some(value) = object.get("max_requests").and_then(|v| v.as_u64()) {
        config.max_requests = value;
    }
    if let Some(value) = object.get("temp_unit").and_then(|v| v.as_str()) {
        config.temp_unit = TempUnit::parse(value).as_str().to_owned();
    }
    if let Some(panels) = object.get("split_panels").and_then(|v| v.as_object()) {
        config.split_panels = parse_split_panels(panels);
    }
    if let Some(value) = object.get("control_split").and_then(|v| v.as_f64()) {
        config.control_split = value as f32;
    }
    if let Some(columns) = object.get("table_columns").and_then(|v| v.as_object()) {
        config.table_columns = parse_table_columns(columns);
    }
    if let Some(state) = object.get("window_state").and_then(|v| v.as_object()) {
        config.window_state = parse_window_state(state);
    }
    if let Some(value) = object.get("update_check_enabled").and_then(|v| v.as_bool()) {
        config.update_check_enabled = value;
    }
    if let Some(value) = object
        .get("update_check_period_days")
        .and_then(|v| v.as_u64())
    {
        config.update_check_period_days = value;
    }
    if let Some(value) = object
        .get("last_update_check_unix_ms")
        .and_then(|v| v.as_u64())
    {
        config.last_update_check_unix_ms = Some(value);
    }
    if let Some(value) = object.get("configs") {
        config.configs = parse_configs(value);
    }
    if let Some(value) = object
        .get("running_config")
        .and_then(|v| v.as_str())
        .filter(|v| !v.trim().is_empty())
    {
        config.running_config = Some(value.to_owned());
    }
    (config, ConfigFileState::Valid)
}

/// Load the config from `path` (see `load_with_state`).
pub fn load(path: &Path) -> Config {
    load_with_state(path).0
}

/// Tolerantly parse the `configs` field (v0.3 M3): a JSON array of objects
/// with `name` and `command_line` strings. A missing or non-array value
/// yields an empty list; a non-object entry is skipped independently of the
/// others; a missing or wrong-typed field inside an entry falls back to the
/// empty string.
pub fn parse_configs(value: &serde_json::Value) -> Vec<ConfigEntry> {
    let Some(array) = value.as_array() else {
        return Vec::new();
    };
    array
        .iter()
        .filter_map(|entry| entry.as_object())
        .map(|object| ConfigEntry {
            name: object
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_owned(),
            command_line: object
                .get("command_line")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_owned(),
        })
        .collect()
}

/// The one-time v0.2 → v0.3 migration of a legacy `last_path` (v0.3 M2):
/// when the path names `server.requests.jsonl` inside a `logs` directory,
/// the grandparent directory is the NInfer installation folder; otherwise
/// the path is not an install folder and is dropped.
fn legacy_install_folder(last_path: &str) -> Option<String> {
    let path = Path::new(last_path);
    let is_log_file = path
        .file_name()
        .is_some_and(|name| name == "server.requests.jsonl");
    let is_logs_dir = path
        .parent()
        .and_then(|p| p.file_name())
        .is_some_and(|name| name == "logs");
    if is_log_file && is_logs_dir {
        path.parent()
            .and_then(|p| p.parent())
            .filter(|p| !p.as_os_str().is_empty())
            .map(|p| p.to_string_lossy().to_string())
    } else {
        None
    }
}

/// Whether the file at `path` is malformed (present but not a JSON object)
/// and should be repaired by a `save` of the defaults. A missing file does
/// not need repair.
#[cfg(test)]
fn needs_repair(path: &Path) -> bool {
    matches!(load_with_state(path).1, ConfigFileState::Malformed)
}

/// The per-process temp file used by `save` for `path`: the target name
/// plus the process id, so concurrent instances cannot clobber each other.
fn temp_path_for(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or("config");
    path.with_file_name(format!("{file_name}.{}.tmp", std::process::id()))
}

/// How old a temp file must be before `cleanup_temp_files` removes it. A
/// `save` is fast, so a minute safely excludes a temp file that is currently
/// being written (by this or another instance).
pub const TEMP_STALE_AFTER: std::time::Duration = std::time::Duration::from_secs(60);

/// Whether `name` is a temp file for `target` that is old enough to be
/// considered stale: it matches the `<target>.<pid>.tmp` pattern and its
/// modification time is more than `stale_after` before `now`.
fn is_stale_temp(
    name: &str,
    target: &str,
    modified: std::time::SystemTime,
    now: std::time::SystemTime,
    stale_after: std::time::Duration,
) -> bool {
    let prefix = format!("{target}.");
    let Some(inner) = name
        .strip_prefix(&prefix)
        .and_then(|rest| rest.strip_suffix(".tmp"))
    else {
        return false;
    };
    !inner.is_empty()
        && inner.bytes().all(|b| b.is_ascii_digit())
        && now.duration_since(modified).is_ok_and(|d| d > stale_after)
}

/// Remove stale temp files left behind by a crashed `save` (a crash between
/// the temp write and the rename leaves `config.json.<pid>.tmp` behind).
/// Only temp files older than [`TEMP_STALE_AFTER`] are removed, so a temp
/// file that is currently being written is not touched.
pub fn cleanup_temp_files(path: &Path) {
    let Some(parent) = path.parent() else {
        return;
    };
    let Some(file_name) = path.file_name().and_then(|f| f.to_str()) else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    let now = std::time::SystemTime::now();
    for entry in entries.flatten() {
        let entry_name = entry.file_name();
        let Some(name) = entry_name.to_str() else {
            continue;
        };
        let Ok(modified) = entry.metadata().and_then(|m| m.modified()) else {
            continue;
        };
        if is_stale_temp(name, file_name, modified, now, TEMP_STALE_AFTER) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Save the config to `path`, creating the parent directory if needed.
///
/// The write goes to a temp file that is renamed over the target, so a
/// crash mid-write cannot leave a truncated config behind.
pub fn save(path: &Path, config: &Config) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let data = serde_json::to_string_pretty(config)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let tmp = temp_path_for(path);
    let mut file = std::fs::File::create(&tmp)?;
    std::io::Write::write_all(&mut file, data.as_bytes())?;
    file.sync_all()?;
    if let Err(error) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(error);
    }
    Ok(())
}

/// Serialize the config writers (load-modify-save) so concurrent writers —
/// the event-loop settings/window/column writers and the off-thread
/// update-check writer — cannot interleave their loads and saves and lose
/// each other's field changes. Hold the guard from the `load` through the
/// `save`. A poisoned lock is recovered so a panicking writer cannot wedge
/// every later config write.
pub fn write_lock() -> std::sync::MutexGuard<'static, ()> {
    static WRITE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    WRITE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_config() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CONFIG_DIR).join(CONFIG_FILE);
        (dir, path)
    }

    #[test]
    fn defaults_match_prd() {
        let c = Config::default();
        assert_eq!(c.install_folder, None);
        assert_eq!(c.log_file_folder, default_log_file_folder());
        assert_eq!(
            c.log_poll_interval_ms,
            DEFAULT_LOG_POLL_INTERVAL.as_millis() as u64
        );
        assert_eq!(c.gpu_poll_interval_ms, DEFAULT_GPU_POLL_MS);
        assert_eq!(c.chart_window_ms, DEFAULT_WINDOW_MS);
        assert_eq!(c.max_requests, DEFAULT_MAX_REQUESTS);
        assert_eq!(c.temp_unit, "F");
        assert_eq!(c.log_poll_interval(), DEFAULT_LOG_POLL_INTERVAL);
        assert_eq!(
            c.gpu_poll_interval(),
            Duration::from_millis(DEFAULT_GPU_POLL_MS)
        );
        assert_eq!(c.chart_window_ms(), DEFAULT_WINDOW_MS);
        assert_eq!(c.max_requests(), DEFAULT_MAX_REQUESTS);
        assert_eq!(c.temp_unit(), TempUnit::Fahrenheit);
    }

    #[test]
    fn load_missing_file_returns_defaults() {
        let (_dir, path) = temp_config();
        assert_eq!(load(&path), Config::default());
    }

    #[test]
    fn load_malformed_json_returns_defaults() {
        let (_dir, path) = temp_config();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{not json").unwrap();
        assert_eq!(load(&path), Config::default());
    }

    #[test]
    fn load_partial_config_fills_missing_fields() {
        let (_dir, path) = temp_config();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"install_folder":"C:/ninfer"}"#).unwrap();
        let c = load(&path);
        assert_eq!(c.install_folder, Some("C:/ninfer".to_owned()));
        assert_eq!(
            c.log_poll_interval_ms,
            DEFAULT_LOG_POLL_INTERVAL.as_millis() as u64
        );
        assert_eq!(c.gpu_poll_interval_ms, DEFAULT_GPU_POLL_MS);
        assert_eq!(c.chart_window_ms, DEFAULT_WINDOW_MS);
        assert_eq!(c.max_requests, DEFAULT_MAX_REQUESTS);
    }

    #[test]
    fn load_log_file_folder_from_file() {
        let (_dir, path) = temp_config();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"log_file_folder":"C:/logs"}"#).unwrap();
        assert_eq!(load(&path).log_file_folder, "C:/logs");
        // Wrong type -> default.
        std::fs::write(&path, r#"{"log_file_folder":42}"#).unwrap();
        assert_eq!(load(&path).log_file_folder, default_log_file_folder());
        // An empty or blank value is normalized to the default.
        std::fs::write(&path, r#"{"log_file_folder":""}"#).unwrap();
        assert_eq!(load(&path).log_file_folder, default_log_file_folder());
        std::fs::write(&path, r#"{"log_file_folder":"   "}"#).unwrap();
        assert_eq!(load(&path).log_file_folder, default_log_file_folder());
    }

    #[test]
    fn load_migrates_legacy_last_path_to_install_folder() {
        let (_dir, path) = temp_config();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{"last_path":"C:/ninfer/logs/server.requests.jsonl"}"#,
        )
        .unwrap();
        assert_eq!(load(&path).install_folder, Some("C:/ninfer".to_owned()));
        // Unix separators are handled the same way.
        std::fs::write(
            &path,
            r#"{"last_path":"/opt/ninfer/logs/server.requests.jsonl"}"#,
        )
        .unwrap();
        assert_eq!(load(&path).install_folder, Some("/opt/ninfer".to_owned()));
    }

    #[test]
    fn load_drops_legacy_last_path_that_is_not_a_log_file() {
        let (_dir, path) = temp_config();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // Not the log file name.
        std::fs::write(&path, r#"{"last_path":"C:/ninfer/logs/other.jsonl"}"#).unwrap();
        assert_eq!(load(&path).install_folder, None);
        // Not inside a `logs` directory.
        std::fs::write(
            &path,
            r#"{"last_path":"C:/elsewhere/server.requests.jsonl"}"#,
        )
        .unwrap();
        assert_eq!(load(&path).install_folder, None);
        // A bare file name has no install folder to derive.
        std::fs::write(&path, r#"{"last_path":"server.requests.jsonl"}"#).unwrap();
        assert_eq!(load(&path).install_folder, None);
    }

    #[test]
    fn load_install_folder_takes_precedence_over_legacy_last_path() {
        let (_dir, path) = temp_config();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{"install_folder":"C:/a","last_path":"C:/b/logs/server.requests.jsonl"}"#,
        )
        .unwrap();
        assert_eq!(load(&path).install_folder, Some("C:/a".to_owned()));
    }

    #[test]
    fn load_blank_install_folder_is_none() {
        let (_dir, path) = temp_config();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"install_folder":""}"#).unwrap();
        assert_eq!(load(&path).install_folder, None);
        std::fs::write(&path, r#"{"install_folder":"   "}"#).unwrap();
        assert_eq!(load(&path).install_folder, None);
        // A blank install folder still suppresses the legacy last_path
        // migration (the v0.3 key is present).
        std::fs::write(
            &path,
            r#"{"install_folder":"","last_path":"C:/b/logs/server.requests.jsonl"}"#,
        )
        .unwrap();
        assert_eq!(load(&path).install_folder, None);
    }

    #[test]
    fn legacy_install_folder_requires_a_grandparent() {
        assert_eq!(
            legacy_install_folder("C:/ninfer/logs/server.requests.jsonl"),
            Some("C:/ninfer".to_owned())
        );
        assert_eq!(legacy_install_folder("logs/server.requests.jsonl"), None);
        assert_eq!(legacy_install_folder("server.requests.jsonl"), None);
    }

    #[test]
    fn load_max_requests_from_file() {
        let (_dir, path) = temp_config();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"max_requests":250}"#).unwrap();
        let c = load(&path);
        assert_eq!(c.max_requests, 250);
        assert_eq!(c.max_requests(), 250);
    }

    #[test]
    fn load_gpu_poll_interval_from_file() {
        let (_dir, path) = temp_config();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"gpu_poll_interval_ms":3000}"#).unwrap();
        let c = load(&path);
        assert_eq!(c.gpu_poll_interval_ms, 3_000);
        assert_eq!(c.gpu_poll_interval(), Duration::from_millis(3_000));
    }

    #[test]
    fn load_gpu_poll_interval_defaults_and_normalizes() {
        let (_dir, path) = temp_config();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // Absent -> default.
        std::fs::write(&path, r#"{"max_requests":250}"#).unwrap();
        assert_eq!(load(&path).gpu_poll_interval_ms, DEFAULT_GPU_POLL_MS);
        // Wrong type -> default.
        std::fs::write(&path, r#"{"gpu_poll_interval_ms":"fast"}"#).unwrap();
        assert_eq!(load(&path).gpu_poll_interval_ms, DEFAULT_GPU_POLL_MS);
    }

    #[test]
    fn load_ignores_renamed_poll_interval_field() {
        // The pre-rename `poll_interval_ms` key is no longer read; it is
        // treated as an unknown field and the default is kept.
        let (_dir, path) = temp_config();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"poll_interval_ms":200}"#).unwrap();
        let c = load(&path);
        assert_eq!(
            c.log_poll_interval_ms,
            DEFAULT_LOG_POLL_INTERVAL.as_millis() as u64
        );
    }

    #[test]
    fn load_temp_unit_from_file() {
        let (_dir, path) = temp_config();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"temp_unit":"C"}"#).unwrap();
        let c = load(&path);
        assert_eq!(c.temp_unit, "C");
        assert_eq!(c.temp_unit(), TempUnit::Celsius);
    }

    #[test]
    fn load_temp_unit_defaults_and_normalizes() {
        let (_dir, path) = temp_config();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // Absent -> default Fahrenheit.
        std::fs::write(&path, r#"{"max_requests":250}"#).unwrap();
        assert_eq!(load(&path).temp_unit, "F");
        // Unknown value -> normalized to the default (Fahrenheit).
        std::fs::write(&path, r#"{"temp_unit":"kelvin"}"#).unwrap();
        assert_eq!(load(&path).temp_unit, "F");
        // Wrong type -> default Fahrenheit.
        std::fs::write(&path, r#"{"temp_unit":42}"#).unwrap();
        assert_eq!(load(&path).temp_unit, "F");
    }

    #[test]
    fn load_update_check_settings_from_file() {
        let (_dir, path) = temp_config();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{"update_check_enabled":false,"update_check_period_days":7,"last_update_check_unix_ms":123456789}"#,
        )
        .unwrap();
        let c = load(&path);
        assert!(!c.update_check_enabled);
        assert_eq!(c.update_check_period_days, 7);
        assert_eq!(c.update_check_period_days(), 7);
        assert_eq!(c.last_update_check_unix_ms, Some(123_456_789));
    }

    #[test]
    fn load_update_check_settings_defaults_and_normalizes() {
        let (_dir, path) = temp_config();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // Absent -> defaults (enabled, 3 days, never checked).
        std::fs::write(&path, r#"{"max_requests":250}"#).unwrap();
        let c = load(&path);
        assert!(c.update_check_enabled);
        assert_eq!(
            c.update_check_period_days,
            crate::update_check::DEFAULT_PERIOD_DAYS
        );
        assert_eq!(c.last_update_check_unix_ms, None);
        // Wrong types -> defaults (the other fields are kept).
        std::fs::write(
            &path,
            r#"{"max_requests":250,"update_check_enabled":"yes","update_check_period_days":"weekly","last_update_check_unix_ms":null}"#,
        )
        .unwrap();
        let c = load(&path);
        assert_eq!(c.max_requests, 250);
        assert!(c.update_check_enabled);
        assert_eq!(
            c.update_check_period_days,
            crate::update_check::DEFAULT_PERIOD_DAYS
        );
        assert_eq!(c.last_update_check_unix_ms, None);
    }

    #[test]
    fn load_update_check_period_out_of_range_kept_and_clamped_on_access() {
        let (_dir, path) = temp_config();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"update_check_period_days":0}"#).unwrap();
        let c = load(&path);
        assert_eq!(c.update_check_period_days, 0);
        assert_eq!(
            c.update_check_period_days(),
            crate::update_check::MIN_PERIOD_DAYS
        );
        std::fs::write(&path, r#"{"update_check_period_days":99}"#).unwrap();
        let c = load(&path);
        assert_eq!(
            c.update_check_period_days(),
            crate::update_check::MAX_PERIOD_DAYS
        );
    }

    #[test]
    fn load_split_panels_from_file() {
        let (_dir, path) = temp_config();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"split_panels":{"charts":0.7,"table":0.3}}"#).unwrap();
        let c = load(&path);
        assert_eq!(
            c.split_panels,
            SplitPanels {
                charts: 0.7,
                table: 0.3
            }
        );
    }

    #[test]
    fn load_split_panels_defaults_when_absent() {
        let (_dir, path) = temp_config();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"max_requests":250}"#).unwrap();
        let c = load(&path);
        assert_eq!(c.split_panels, SplitPanels::default());
    }

    #[test]
    fn load_split_panels_defaults_wrong_typed_fields() {
        let (_dir, path) = temp_config();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"split_panels":{"charts":"wide","table":[1]}}"#).unwrap();
        let c = load(&path);
        assert_eq!(c.split_panels, SplitPanels::default());
    }

    #[test]
    fn load_split_panels_ignores_non_object() {
        let (_dir, path) = temp_config();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"split_panels":42}"#).unwrap();
        let c = load(&path);
        assert_eq!(c.split_panels, SplitPanels::default());
    }

    #[test]
    fn load_control_split_from_file() {
        let (_dir, path) = temp_config();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"control_split":0.3}"#).unwrap();
        let c = load(&path);
        assert_eq!(c.control_split, 0.3);
        assert_eq!(c.control_split(), 0.3);
    }

    #[test]
    fn load_control_split_defaults_when_absent() {
        let (_dir, path) = temp_config();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"max_requests":250}"#).unwrap();
        let c = load(&path);
        assert_eq!(c.control_split, DEFAULT_CONTROL_SPLIT);
    }

    #[test]
    fn load_control_split_defaults_wrong_typed_field() {
        let (_dir, path) = temp_config();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"control_split":"wide"}"#).unwrap();
        let c = load(&path);
        assert_eq!(c.control_split, DEFAULT_CONTROL_SPLIT);
    }

    #[test]
    fn control_split_clamps_out_of_range() {
        let c = Config {
            control_split: 0.0,
            ..Default::default()
        };
        assert_eq!(c.control_split(), MIN_CONTROL_SPLIT);
        let c = Config {
            control_split: 0.99,
            ..Default::default()
        };
        assert_eq!(c.control_split(), MAX_CONTROL_SPLIT);
        let c = Config {
            control_split: 0.4,
            ..Default::default()
        };
        assert_eq!(c.control_split(), 0.4);
    }

    #[test]
    fn load_configs_from_file() {
        let (_dir, path) = temp_config();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{"configs":[{"name":"my-server","command_line":"--model x --port 8080"},{"name":"bench-run","command_line":""}]}"#,
        )
        .unwrap();
        let c = load(&path);
        assert_eq!(
            c.configs,
            vec![
                ConfigEntry {
                    name: "my-server".to_owned(),
                    command_line: "--model x --port 8080".to_owned(),
                },
                ConfigEntry {
                    name: "bench-run".to_owned(),
                    command_line: String::new(),
                },
            ]
        );
    }

    #[test]
    fn load_configs_defaults_to_empty_when_absent() {
        let (_dir, path) = temp_config();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"max_requests":250}"#).unwrap();
        let c = load(&path);
        assert!(c.configs.is_empty());
        assert_eq!(c.max_requests, 250);
    }

    #[test]
    fn load_configs_non_array_is_empty() {
        let (_dir, path) = temp_config();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"configs":42}"#).unwrap();
        assert!(load(&path).configs.is_empty());
        std::fs::write(&path, r#"{"configs":{"name":"x"}}"#).unwrap();
        assert!(load(&path).configs.is_empty());
    }

    #[test]
    fn load_configs_skips_malformed_entries_independently() {
        let (_dir, path) = temp_config();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // A non-object entry (a string) is skipped; the valid entries around
        // it are kept.
        std::fs::write(
            &path,
            r#"{"configs":["junk",{"name":"a","command_line":"--x"},42,{"name":"b","command_line":"--y"}]}"#,
        )
        .unwrap();
        let c = load(&path);
        assert_eq!(
            c.configs,
            vec![
                ConfigEntry {
                    name: "a".to_owned(),
                    command_line: "--x".to_owned(),
                },
                ConfigEntry {
                    name: "b".to_owned(),
                    command_line: "--y".to_owned(),
                },
            ]
        );
    }

    #[test]
    fn load_configs_defaults_wrong_typed_fields() {
        let (_dir, path) = temp_config();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // A missing or wrong-typed field inside an entry falls back to the
        // empty string; the entry itself is kept.
        std::fs::write(
            &path,
            r#"{"configs":[{"name":42,"command_line":"--x"},{"command_line":7},{"name":"c"}]}"#,
        )
        .unwrap();
        let c = load(&path);
        assert_eq!(
            c.configs,
            vec![
                ConfigEntry {
                    name: String::new(),
                    command_line: "--x".to_owned(),
                },
                ConfigEntry {
                    name: String::new(),
                    command_line: String::new(),
                },
                ConfigEntry {
                    name: "c".to_owned(),
                    command_line: String::new(),
                },
            ]
        );
    }

    #[test]
    fn load_running_config_from_file() {
        let (_dir, path) = temp_config();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"running_config":"my-server"}"#).unwrap();
        assert_eq!(load(&path).running_config, Some("my-server".to_owned()));
        std::fs::write(&path, r#"{"running_config":""}"#).unwrap();
        assert_eq!(load(&path).running_config, None);
        std::fs::write(&path, r#"{"running_config":"   "}"#).unwrap();
        assert_eq!(load(&path).running_config, None);
        std::fs::write(&path, r#"{"running_config":42}"#).unwrap();
        assert_eq!(load(&path).running_config, None);
    }

    #[test]
    fn parse_configs_handles_non_array_and_empty() {
        assert!(parse_configs(&serde_json::json!(42)).is_empty());
        assert!(parse_configs(&serde_json::json!("x")).is_empty());
        assert!(parse_configs(&serde_json::json!(null)).is_empty());
        assert!(parse_configs(&serde_json::json!([])).is_empty());
        assert_eq!(
            parse_configs(&serde_json::json!([{"name":"a","command_line":"--b"}])),
            vec![ConfigEntry {
                name: "a".to_owned(),
                command_line: "--b".to_owned(),
            }]
        );
    }

    #[test]
    fn load_table_columns_from_file() {
        let (_dir, path) = temp_config();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{"table_columns":{"window-width":1264.0,"id":56,"time":120,"model":320,"prompt":72,"compl":72,"think":72,"ttft":72,"total":72,"reason":180,"status":100}}"#,
        )
        .unwrap();
        let c = load(&path);
        assert_eq!(
            c.table_columns,
            Some(TableColumns {
                window_width: 1264.0,
                id: 56,
                time: 120,
                model: 320,
                prompt: 72,
                compl: 72,
                think: 72,
                ttft: 72,
                total: 72,
                reason: 180,
                status: 100,
            })
        );
    }

    #[test]
    fn load_table_columns_defaults_when_absent_or_malformed() {
        let (_dir, path) = temp_config();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // Absent -> default.
        std::fs::write(&path, r#"{"max_requests":250}"#).unwrap();
        assert_eq!(load(&path).table_columns, None);
        // A missing width field -> default (the other fields are kept).
        std::fs::write(
            &path,
            r#"{"max_requests":250,"table_columns":{"window-width":1280.0,"id":56}}"#,
        )
        .unwrap();
        let c = load(&path);
        assert_eq!(c.table_columns, None);
        assert_eq!(c.max_requests, 250);
        // A non-positive window width -> default.
        std::fs::write(
            &path,
            r#"{"table_columns":{"window-width":0.0,"id":56,"time":105,"model":456,"prompt":60,"compl":60,"think":65,"ttft":65,"total":60,"reason":150,"status":75}}"#,
        )
        .unwrap();
        assert_eq!(load(&path).table_columns, None);
        // A non-object entry -> default.
        std::fs::write(&path, r#"{"table_columns":42}"#).unwrap();
        assert_eq!(load(&path).table_columns, None);
    }

    #[test]
    fn load_ignores_unknown_fields() {
        let (_dir, path) = temp_config();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"future_setting":42}"#).unwrap();
        let c = load(&path);
        assert_eq!(c.install_folder, None);
    }

    #[test]
    fn load_wrong_typed_field_keeps_other_fields() {
        let (_dir, path) = temp_config();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{"install_folder":"C:/ninfer","log_poll_interval_ms":"fast"}"#,
        )
        .unwrap();
        let c = load(&path);
        assert_eq!(c.install_folder, Some("C:/ninfer".to_owned()));
        assert_eq!(
            c.log_poll_interval_ms,
            DEFAULT_LOG_POLL_INTERVAL.as_millis() as u64
        );
        assert_eq!(c.chart_window_ms, DEFAULT_WINDOW_MS);
    }

    #[test]
    fn load_non_object_json_returns_defaults() {
        let (_dir, path) = temp_config();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "[1, 2, 3]").unwrap();
        assert_eq!(load(&path), Config::default());
    }

    #[test]
    fn load_non_utf8_file_is_malformed() {
        let (_dir, path) = temp_config();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // 0xFF 0xFE is not valid UTF-8.
        std::fs::write(&path, [0xFFu8, 0xFE, 0x00, 0x01]).unwrap();
        let (cfg, state) = load_with_state(&path);
        assert_eq!(cfg, Config::default());
        assert_eq!(state, ConfigFileState::Malformed);
        assert!(needs_repair(&path), "a non-UTF-8 file needs repair");
    }

    #[test]
    fn out_of_range_values_are_clamped() {
        let c = Config {
            install_folder: None,
            log_file_folder: default_log_file_folder(),
            log_poll_interval_ms: 1,
            gpu_poll_interval_ms: 1,
            chart_window_ms: 999_999_999,
            max_requests: 1,
            temp_unit: "F".to_owned(),
            split_panels: SplitPanels::default(),
            control_split: DEFAULT_CONTROL_SPLIT,
            table_columns: None,
            window_state: None,
            update_check_enabled: true,
            update_check_period_days: 0,
            last_update_check_unix_ms: None,
            configs: Vec::new(),
            running_config: None,
        };
        assert_eq!(
            c.log_poll_interval(),
            Duration::from_millis(MIN_LOG_POLL_MS)
        );
        assert_eq!(
            c.gpu_poll_interval(),
            Duration::from_millis(MIN_GPU_POLL_MS)
        );
        assert_eq!(c.chart_window_ms(), MAX_WINDOW_MS);
        assert_eq!(c.max_requests(), MIN_MAX_REQUESTS);
        assert_eq!(
            c.update_check_period_days(),
            crate::update_check::MIN_PERIOD_DAYS
        );

        let c = Config {
            install_folder: None,
            log_file_folder: default_log_file_folder(),
            log_poll_interval_ms: 999_999,
            gpu_poll_interval_ms: 999_999,
            chart_window_ms: 1_000,
            max_requests: 999_999,
            temp_unit: "F".to_owned(),
            split_panels: SplitPanels::default(),
            control_split: DEFAULT_CONTROL_SPLIT,
            table_columns: None,
            window_state: None,
            update_check_enabled: true,
            update_check_period_days: 999,
            last_update_check_unix_ms: None,
            configs: Vec::new(),
            running_config: None,
        };
        assert_eq!(
            c.log_poll_interval(),
            Duration::from_millis(MAX_LOG_POLL_MS)
        );
        assert_eq!(
            c.gpu_poll_interval(),
            Duration::from_millis(MAX_GPU_POLL_MS)
        );
        assert_eq!(c.chart_window_ms(), MIN_WINDOW_MS);
        assert_eq!(c.max_requests(), MAX_MAX_REQUESTS);
        assert_eq!(
            c.update_check_period_days(),
            crate::update_check::MAX_PERIOD_DAYS
        );
    }

    #[test]
    fn save_creates_parent_dirs_and_round_trips() {
        let (_dir, path) = temp_config();
        let c = Config {
            install_folder: Some("C:/ninfer".to_owned()),
            log_file_folder: "C:/logs".to_owned(),
            log_poll_interval_ms: 100,
            gpu_poll_interval_ms: 3_000,
            chart_window_ms: 900_000,
            max_requests: 250,
            temp_unit: "F".to_owned(),
            split_panels: SplitPanels {
                charts: 0.7,
                table: 0.3,
            },
            control_split: 0.6,
            table_columns: None,
            window_state: None,
            update_check_enabled: false,
            update_check_period_days: 7,
            last_update_check_unix_ms: Some(1_700_000_000_000),
            configs: vec![
                ConfigEntry {
                    name: "my-server".to_owned(),
                    command_line: "--model qwen3.8-27b-nvfp4 --port 8080".to_owned(),
                },
                ConfigEntry {
                    name: "bench-run".to_owned(),
                    command_line: String::new(),
                },
            ],
            running_config: Some("my-server".to_owned()),
        };
        save(&path, &c).unwrap();
        assert_eq!(load(&path), c);
    }

    #[test]
    fn save_overwrites_existing() {
        let (_dir, path) = temp_config();
        save(&path, &Config::default()).unwrap();
        let c = Config {
            install_folder: Some("y".to_owned()),
            log_file_folder: "C:/logs2".to_owned(),
            log_poll_interval_ms: 200,
            gpu_poll_interval_ms: 7_000,
            chart_window_ms: 60_000,
            max_requests: 500,
            temp_unit: "F".to_owned(),
            split_panels: SplitPanels {
                charts: 0.4,
                table: 0.6,
            },
            control_split: 0.3,
            table_columns: None,
            window_state: None,
            update_check_enabled: true,
            update_check_period_days: 14,
            last_update_check_unix_ms: Some(1_700_000_123_456),
            configs: vec![ConfigEntry {
                name: "only".to_owned(),
                command_line: "--port 9090".to_owned(),
            }],
            running_config: None,
        };
        save(&path, &c).unwrap();
        assert_eq!(load(&path), c);
    }

    #[test]
    fn save_leaves_no_temp_file() {
        let (_dir, path) = temp_config();
        save(&path, &Config::default()).unwrap();
        assert!(!temp_path_for(&path).exists());
    }

    #[test]
    fn is_stale_temp_matches_pattern_and_age() {
        let now = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1000);
        let fresh = now - Duration::from_secs(10);
        let stale = now - Duration::from_secs(120);
        let cutoff = Duration::from_secs(60);
        assert!(
            is_stale_temp("config.json.123.tmp", "config.json", stale, now, cutoff),
            "a stale temp file is matched"
        );
        assert!(
            !is_stale_temp("config.json.123.tmp", "config.json", fresh, now, cutoff),
            "a fresh temp file is not stale"
        );
        assert!(
            !is_stale_temp("config.json", "config.json", stale, now, cutoff),
            "the config file itself is not a temp file"
        );
        assert!(
            !is_stale_temp("other.json.123.tmp", "config.json", stale, now, cutoff),
            "a temp file for another target is not matched"
        );
        assert!(
            !is_stale_temp("config.json.123", "config.json", stale, now, cutoff),
            "a file without the .tmp suffix is not matched"
        );
        assert!(
            !is_stale_temp("config.json.backup.tmp", "config.json", stale, now, cutoff),
            "a non-pid middle segment is not matched"
        );
        assert!(
            !is_stale_temp("config.json.tmp", "config.json", stale, now, cutoff),
            "a missing pid segment is not matched"
        );
    }

    #[test]
    fn cleanup_temp_files_keeps_fresh_temp_and_config() {
        let (_dir, path) = temp_config();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        save(&path, &Config::default()).unwrap();
        // A fresh temp file (just created, < a minute old) must not be removed.
        let fresh_tmp = temp_path_for(&path);
        std::fs::write(&fresh_tmp, "{}").unwrap();
        cleanup_temp_files(&path);
        assert!(fresh_tmp.exists(), "a fresh temp file is kept");
        assert!(path.exists(), "the config file is kept");
    }

    #[test]
    fn default_config_path_lives_in_home_dot_dir() {
        let path = Path::new("/home/user").join(CONFIG_DIR).join(CONFIG_FILE);
        assert_eq!(path, Path::new("/home/user/.ninfer-monitor/config.json"));
    }

    #[test]
    fn default_config_path_uses_platform_home() {
        let path = default_config_path().expect("home dir on this platform");
        // The path must end with `.ninfer-monitor/config.json` and live under
        // the platform home directory (not just mirror the implementation).
        let suffix = Path::new(CONFIG_DIR).join(CONFIG_FILE);
        assert!(
            path.ends_with(&suffix),
            "the config path must end with {}/{}, got {}",
            CONFIG_DIR,
            CONFIG_FILE,
            path.display()
        );
        let home = home_dir().expect("home dir");
        assert!(
            path.starts_with(&home),
            "the config path must be under the home dir, got {}",
            path.display()
        );
    }

    #[test]
    fn needs_repair_detects_malformed_file() {
        let (_dir, path) = temp_config();
        assert!(!needs_repair(&path), "a missing file does not need repair");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{not json").unwrap();
        assert!(needs_repair(&path), "a malformed file needs repair");
        std::fs::write(&path, "[1, 2, 3]").unwrap();
        assert!(needs_repair(&path), "a non-object JSON file needs repair");
        std::fs::write(&path, r#"{"log_poll_interval_ms":200}"#).unwrap();
        assert!(
            !needs_repair(&path),
            "a valid object file does not need repair"
        );
    }
}
