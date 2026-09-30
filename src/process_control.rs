// NInferMonitor
//
// This is free software; see the LICENSE.md file in the source distribution for precise wording.
//
// Copyright (C) 2026 Aleksey Sanin aleksey@aleksey.com. All Rights Reserved.

use std::path::{Path, PathBuf};

/// The request-log option scanned in the command line (v0.3 M4, PRD §4.2).
const REQUEST_LOG_JSONL: &str = "--request-log-jsonl";

/// Tokenize a command line (v0.3 M4, PRD §4.2): split into an argument
/// vector on whitespace. Newlines and carriage returns are replaced with
/// spaces, so the options can be laid out across multiple lines. Double
/// quotes group characters into a single argument (the quotes are removed);
/// a backslash immediately before a double quote produces a literal quote
/// inside the argument; a backslash not before a double quote is a literal
/// backslash. There is no shell interpretation (no pipes, redirects,
/// environment-variable expansion, or globbing). Empty or whitespace-only
/// input yields an empty vector.
pub fn tokenize(command_line: &str) -> Vec<String> {
    let normalized: String = command_line
        .chars()
        .map(|c| match c {
            '\n' | '\r' => ' ',
            other => other,
        })
        .collect();
    let mut args: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut has_token = false;
    let mut chars = normalized.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&'"') => {
                current.push('"');
                chars.next();
                has_token = true;
            }
            '"' => {
                in_quotes = !in_quotes;
                has_token = true;
            }
            c if c.is_whitespace() && !in_quotes => {
                if has_token {
                    args.push(std::mem::take(&mut current));
                    has_token = false;
                }
            }
            c => {
                current.push(c);
                has_token = true;
            }
        }
    }
    if has_token {
        args.push(current);
    }
    args
}

/// The NInfer server executable in the installation folder (v0.3 M4, PRD
/// §4.2): `<install_folder>/ninfer-serve` with the OS-specific extension.
pub fn program_path(install_folder: &Path) -> PathBuf {
    install_folder.join(crate::startup::ninfer_serve_exe_name())
}

/// Determine the argument vector and the log file to monitor (v0.3 M4,
/// PRD §4.2): scan `args` for `--request-log-jsonl` (the
/// `--request-log-jsonl <path>` and `--request-log-jsonl=<path>` forms; the
/// last occurrence with a value wins). If present, the path is resolved (a
/// relative path against `install_folder`, an absolute path as-is) and the
/// arguments are returned unchanged (not app-managed). If absent (a
/// dangling option or an empty `--request-log-jsonl=` is treated as
/// absent), the option tokens are dropped and `--request-log-jsonl
/// <log_file_folder>/ninfer-monitor/server.requests.jsonl` is appended to
/// the arguments; the app-managed path is returned. The log directory is
/// not created here (the caller creates it).
pub fn determine_log_file(
    args: &[String],
    install_folder: &Path,
    log_file_folder: &Path,
) -> (Vec<String>, PathBuf, bool) {
    let eq_form = format!("{REQUEST_LOG_JSONL}=");
    let mut value: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if let Some(v) = arg.strip_prefix(&eq_form) {
            if !v.is_empty() {
                value = Some(v.to_owned());
            }
        } else if *arg == REQUEST_LOG_JSONL && i + 1 < args.len() {
            value = Some(args[i + 1].clone());
            i += 1;
        }
        i += 1;
    }
    match value {
        Some(value) => (
            args.to_vec(),
            resolve_log_path(&value, install_folder),
            false,
        ),
        None => {
            // The option is absent: the dangling option tokens are treated
            // as absent (dropped), and the app-managed log file is appended.
            let new_args: Vec<String> = args
                .iter()
                .filter(|arg| arg.as_str() != REQUEST_LOG_JSONL && !arg.starts_with(&eq_form))
                .cloned()
                .collect();
            let log_path = crate::startup::monitored_log_path(&log_file_folder.to_string_lossy());
            let mut new_args = new_args;
            new_args.push(REQUEST_LOG_JSONL.to_owned());
            new_args.push(log_path.to_string_lossy().to_string());
            (new_args, log_path, true)
        }
    }
}

/// Resolve a log file path (v0.3 M4): a relative path is resolved against
/// the installation folder (the child process's working directory); an
/// absolute path is used as-is.
fn resolve_log_path(value: &str, install_folder: &Path) -> PathBuf {
    let path = Path::new(value);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        install_folder.join(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn tokenize_splits_on_whitespace() {
        assert_eq!(
            tokenize("--model qwen3.8-27b-nvfp4 --port 8080"),
            s(&["--model", "qwen3.8-27b-nvfp4", "--port", "8080"])
        );
    }

    #[test]
    fn tokenize_collapses_repeated_whitespace() {
        assert_eq!(tokenize("a  b\t\nc"), s(&["a", "b", "c"]));
    }

    #[test]
    fn tokenize_replaces_newlines_with_spaces() {
        assert_eq!(
            tokenize("--model qwen3.8-27b-nvfp4\n--port 8080\r\n--host 0.0.0.0"),
            s(&[
                "--model",
                "qwen3.8-27b-nvfp4",
                "--port",
                "8080",
                "--host",
                "0.0.0.0"
            ])
        );
    }

    #[test]
    fn tokenize_replaces_newlines_inside_quotes_with_spaces() {
        assert_eq!(
            tokenize("--prompt \"line1\nline2\""),
            s(&["--prompt", "line1 line2"])
        );
    }

    #[test]
    fn tokenize_empty_and_blank_yield_no_arguments() {
        assert_eq!(tokenize(""), Vec::<String>::new());
        assert_eq!(tokenize("   \t  "), Vec::<String>::new());
    }

    #[test]
    fn tokenize_quotes_group_into_one_argument() {
        assert_eq!(
            tokenize("--model \"qwen 3.8\""),
            s(&["--model", "qwen 3.8"])
        );
    }

    #[test]
    fn tokenize_escaped_quote_is_a_literal_quote() {
        assert_eq!(
            tokenize("--path \"C:\\new\\\"folder\""),
            s(&["--path", "C:\\new\"folder"])
        );
    }

    #[test]
    fn tokenize_backslash_not_before_a_quote_is_literal() {
        assert_eq!(tokenize("C:\\temp\\log"), s(&["C:\\temp\\log"]));
    }

    #[test]
    fn tokenize_trailing_backslash_is_literal() {
        assert_eq!(tokenize("a\\"), s(&["a\\"]));
    }

    #[test]
    fn tokenize_escaped_quote_outside_quotes() {
        assert_eq!(tokenize("a\\\"b"), s(&["a\"b"]));
    }

    #[test]
    fn tokenize_empty_quoted_argument() {
        assert_eq!(tokenize("\"\""), s(&[""]));
        assert_eq!(tokenize("a \"\" b"), s(&["a", "", "b"]));
    }

    #[test]
    fn program_path_has_the_os_specific_extension() {
        #[cfg(windows)]
        {
            assert_eq!(
                program_path(Path::new("C:/ninfer")),
                Path::new("C:/ninfer/ninfer-serve.exe")
            );
        }
        #[cfg(not(windows))]
        {
            assert_eq!(
                program_path(Path::new("/opt/ninfer")),
                Path::new("/opt/ninfer/ninfer-serve")
            );
        }
    }

    #[test]
    fn determine_log_file_with_the_space_form() {
        let install = Path::new("C:/ninfer");
        let lff = Path::new("C:/logs");
        let (args, path, managed) = determine_log_file(
            &s(&[
                "--model",
                "x",
                "--request-log-jsonl",
                "logs/server.requests.jsonl",
            ]),
            install,
            lff,
        );
        assert_eq!(
            args,
            s(&[
                "--model",
                "x",
                "--request-log-jsonl",
                "logs/server.requests.jsonl"
            ])
        );
        assert_eq!(path, Path::new("C:/ninfer/logs/server.requests.jsonl"));
        assert!(!managed);
    }

    #[test]
    fn determine_log_file_with_the_equals_form() {
        let install = Path::new("C:/ninfer");
        let lff = Path::new("C:/logs");
        let (args, path, managed) =
            determine_log_file(&s(&["--request-log-jsonl=logs/a.jsonl"]), install, lff);
        assert_eq!(args, s(&["--request-log-jsonl=logs/a.jsonl"]));
        assert_eq!(path, Path::new("C:/ninfer/logs/a.jsonl"));
        assert!(!managed);
    }

    #[test]
    fn determine_log_file_last_occurrence_wins() {
        let install = Path::new("C:/ninfer");
        let lff = Path::new("C:/logs");
        let (_, path, managed) = determine_log_file(
            &s(&[
                "--request-log-jsonl=a.jsonl",
                "--request-log-jsonl",
                "b.jsonl",
            ]),
            install,
            lff,
        );
        assert_eq!(path, Path::new("C:/ninfer/b.jsonl"));
        assert!(!managed);
    }

    #[test]
    fn determine_log_file_dangling_option_is_absent() {
        let install = Path::new("C:/ninfer");
        let lff = Path::new("C:/logs");
        let (args, path, managed) =
            determine_log_file(&s(&["--model", "x", "--request-log-jsonl"]), install, lff);
        assert!(managed);
        let log_path = Path::new("C:/logs")
            .join("ninfer-monitor")
            .join("server.requests.jsonl");
        assert_eq!(path, log_path);
        let mut expected = s(&["--model", "x", "--request-log-jsonl"]);
        expected.push(log_path.to_string_lossy().to_string());
        assert_eq!(args, expected);
    }

    #[test]
    fn determine_log_file_empty_equals_value_is_absent() {
        let install = Path::new("C:/ninfer");
        let lff = Path::new("C:/logs");
        let (_, path, managed) = determine_log_file(&s(&["--request-log-jsonl="]), install, lff);
        assert!(managed);
        assert_eq!(
            path,
            Path::new("C:/logs/ninfer-monitor/server.requests.jsonl")
        );
    }

    #[test]
    fn determine_log_file_without_the_option_appends_the_app_managed_path() {
        let install = Path::new("C:/ninfer");
        let lff = Path::new("C:/logs");
        let (args, path, managed) = determine_log_file(&s(&["--model", "x"]), install, lff);
        assert!(managed);
        let log_path = Path::new("C:/logs")
            .join("ninfer-monitor")
            .join("server.requests.jsonl");
        assert_eq!(path, log_path);
        let mut expected = s(&["--model", "x", "--request-log-jsonl"]);
        expected.push(log_path.to_string_lossy().to_string());
        assert_eq!(args, expected);
    }

    #[test]
    fn determine_log_file_absolute_path_is_used_as_is() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("a.jsonl");
        let (args, path, managed) = determine_log_file(
            &[format!("--request-log-jsonl={}", log.display())],
            dir.path(),
            dir.path(),
        );
        assert_eq!(path, log);
        assert!(!managed);
        assert_eq!(
            args,
            s(&[format!("--request-log-jsonl={}", log.display()).as_str()])
        );
    }
}
