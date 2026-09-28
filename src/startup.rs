// NInferMonitor
//
// This is free software; see the LICENSE.md file in the source distribution for precise wording.
//
// Copyright (C) 2026 Aleksey Sanin aleksey@aleksey.com. All Rights Reserved.

use std::path::{Path, PathBuf};

/// The startup-screen message shown when no NInfer installation folder is
/// configured (v0.3 M2).
pub const MSG_NO_FOLDER: &str = "Please select the NInfer installation folder to start.";

/// The startup-screen message shown when a configured folder is not a valid
/// NInfer installation folder (v0.3 M2).
pub const MSG_INVALID_FOLDER: &str = "The currently selected folder is not a valid NInfer installation folder (it must contain the ninfer-serve executable), please select the NInfer installation folder to start monitoring.";

/// The file name of the NInfer server executable, with the OS-specific
/// extension (v0.3 M2).
pub fn ninfer_serve_exe_name() -> &'static str {
    #[cfg(windows)]
    {
        "ninfer-serve.exe"
    }
    #[cfg(not(windows))]
    {
        "ninfer-serve"
    }
}

/// Whether `path` is a valid NInfer installation folder: an existing
/// directory that contains the `ninfer-serve` executable (v0.3 M2).
pub fn is_valid_install_folder(path: &Path) -> bool {
    path.join(ninfer_serve_exe_name()).is_file()
}

/// The app-managed log file monitored by default:
/// `<log_file_folder>/ninfer-monitor/server.requests.jsonl` (v0.3 M2).
pub fn monitored_log_path(log_file_folder: &str) -> PathBuf {
    Path::new(log_file_folder)
        .join("ninfer-monitor")
        .join("server.requests.jsonl")
}

/// The startup screen's state (v0.3 M2), derived from the resolved install
/// folder (CLI folder or the configured `install_folder`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartupState {
    /// Whether the startup screen is shown. `false` when a valid install
    /// folder is given, in which case the dashboard opens directly.
    pub visible: bool,
    /// The message to show (empty when the screen is not shown).
    pub message: &'static str,
    /// The folder to pre-fill the text box with (empty when none is
    /// configured).
    pub folder: String,
}

/// Decide the startup screen's state from the resolved install folder
/// (v0.3 M2): a valid folder skips the screen; a configured folder that is
/// not a valid install folder shows the "invalid folder" message with the
/// folder pre-filled; no configured folder shows the "no folder configured"
/// message.
pub fn startup_state(resolved_folder: Option<&Path>) -> StartupState {
    match resolved_folder {
        Some(folder) if is_valid_install_folder(folder) => StartupState {
            visible: false,
            message: "",
            folder: String::new(),
        },
        Some(folder) => StartupState {
            visible: true,
            message: MSG_INVALID_FOLDER,
            folder: folder.display().to_string(),
        },
        None => StartupState {
            visible: true,
            message: MSG_NO_FOLDER,
            folder: String::new(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_folder_shows_no_folder_message() {
        let s = startup_state(None);
        assert!(s.visible);
        assert_eq!(s.message, MSG_NO_FOLDER);
        assert_eq!(s.folder, "");
    }

    #[test]
    fn valid_folder_skips_startup_screen() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::File::create(dir.path().join(ninfer_serve_exe_name())).unwrap();
        let s = startup_state(Some(dir.path()));
        assert!(!s.visible);
        assert_eq!(s.message, "");
        assert_eq!(s.folder, "");
    }

    #[test]
    fn missing_folder_shows_invalid_message() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing");
        let s = startup_state(Some(&path));
        assert!(s.visible);
        assert_eq!(s.message, MSG_INVALID_FOLDER);
        assert_eq!(s.folder, path.display().to_string());
    }

    #[test]
    fn folder_without_the_executable_shows_invalid_message() {
        let dir = tempfile::tempdir().unwrap();
        let s = startup_state(Some(dir.path()));
        assert!(s.visible);
        assert_eq!(s.message, MSG_INVALID_FOLDER);
        assert_eq!(s.folder, dir.path().display().to_string());
    }

    #[test]
    fn is_valid_install_folder_true_when_the_executable_is_present() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::File::create(dir.path().join(ninfer_serve_exe_name())).unwrap();
        assert!(is_valid_install_folder(dir.path()));
    }

    #[test]
    fn is_valid_install_folder_false_for_missing_folder() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_valid_install_folder(&dir.path().join("missing")));
    }

    #[test]
    fn is_valid_install_folder_false_when_the_executable_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_valid_install_folder(dir.path()));
    }

    #[test]
    fn is_valid_install_folder_false_when_the_executable_is_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(ninfer_serve_exe_name())).unwrap();
        assert!(!is_valid_install_folder(dir.path()));
    }

    #[test]
    fn is_valid_install_folder_false_for_empty_path() {
        assert!(!is_valid_install_folder(Path::new("")));
    }

    #[test]
    fn monitored_log_path_joins_the_app_log_file() {
        assert_eq!(
            monitored_log_path("C:/logs"),
            Path::new("C:/logs")
                .join("ninfer-monitor")
                .join("server.requests.jsonl")
        );
        assert_eq!(
            monitored_log_path("/tmp"),
            Path::new("/tmp")
                .join("ninfer-monitor")
                .join("server.requests.jsonl")
        );
    }

    #[cfg(windows)]
    #[test]
    fn exe_name_has_the_windows_extension() {
        assert_eq!(ninfer_serve_exe_name(), "ninfer-serve.exe");
    }

    #[cfg(not(windows))]
    #[test]
    fn exe_name_has_no_extension_off_windows() {
        assert_eq!(ninfer_serve_exe_name(), "ninfer-serve");
    }
}
