// NInferMonitor
//
// This is free software; see the LICENSE.md file in the source distribution for precise wording.
//
// Copyright (C) 2026 Aleksey Sanin aleksey@aleksey.com. All Rights Reserved.

use std::path::Path;

/// The startup-screen message shown when no log file is configured (PRD §3.1).
pub const MSG_NO_FILE: &str =
    "Please select NInfer log file (e.g. 'server.requests.jsonl') to start monitoring.";

/// The startup-screen message shown when a configured log file does not exist
/// or cannot be read (PRD §3.1).
pub const MSG_UNREADABLE: &str = "The currently selected file cannot be read, please ensure NInfer is configured correctly and is running; or select another file to start monitoring.";

/// Whether `path` points to an existing, readable file — the validity
/// criterion for the startup screen's Start button (PRD §3.1).
pub fn file_is_readable(path: &Path) -> bool {
    path.is_file() && std::fs::File::open(path).is_ok()
}

/// The startup screen's state (PRD §3.1/M1), derived from the resolved log
/// path (CLI path or the configured `last_path`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartupState {
    /// Whether the startup screen is shown. `false` when a valid (existing,
    /// readable) file is given, in which case the dashboard opens directly.
    pub visible: bool,
    /// The message to show (empty when the screen is not shown).
    pub message: &'static str,
    /// The file to pre-fill the text box with (empty when none is configured).
    pub file: String,
}

/// Decide the startup screen's state from the resolved log path (PRD §3.1/M1):
/// a valid (existing, readable) file skips the screen; a configured file that
/// is missing or unreadable shows the "cannot be read" message with the file
/// pre-filled; no configured file shows the "no file configured" message.
pub fn startup_state(resolved_path: Option<&Path>) -> StartupState {
    match resolved_path {
        Some(path) if file_is_readable(path) => StartupState {
            visible: false,
            message: "",
            file: String::new(),
        },
        Some(path) => StartupState {
            visible: true,
            message: MSG_UNREADABLE,
            file: path.display().to_string(),
        },
        None => StartupState {
            visible: true,
            message: MSG_NO_FILE,
            file: String::new(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_path_shows_no_file_message() {
        let s = startup_state(None);
        assert!(s.visible);
        assert_eq!(s.message, MSG_NO_FILE);
        assert_eq!(s.file, "");
    }

    #[test]
    fn valid_path_skips_startup_screen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log.jsonl");
        std::fs::File::create(&path).unwrap();
        let s = startup_state(Some(&path));
        assert!(!s.visible);
        assert_eq!(s.message, "");
        assert_eq!(s.file, "");
    }

    #[test]
    fn missing_path_shows_unreadable_message() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.jsonl");
        let s = startup_state(Some(&path));
        assert!(s.visible);
        assert_eq!(s.message, MSG_UNREADABLE);
        assert_eq!(s.file, path.display().to_string());
    }

    #[test]
    fn directory_path_shows_unreadable_message() {
        let dir = tempfile::tempdir().unwrap();
        let s = startup_state(Some(dir.path()));
        assert!(s.visible);
        assert_eq!(s.message, MSG_UNREADABLE);
        assert_eq!(s.file, dir.path().display().to_string());
    }

    #[test]
    fn file_is_readable_true_for_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log.jsonl");
        std::fs::File::create(&path).unwrap();
        assert!(file_is_readable(&path));
    }

    #[test]
    fn file_is_readable_false_for_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.jsonl");
        assert!(!file_is_readable(&path));
    }

    #[test]
    fn file_is_readable_false_for_directory() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!file_is_readable(dir.path()));
    }

    #[test]
    fn file_is_readable_false_for_empty_path() {
        assert!(!file_is_readable(Path::new("")));
    }
}
