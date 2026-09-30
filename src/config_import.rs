// NInferMonitor
//
// This is free software; see the LICENSE.md file in the source distribution for precise wording.
//
// Copyright (C) 2026 Aleksey Sanin aleksey@aleksey.com. All Rights Reserved.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// The launcher script dialect (v0.3): the line-continuation marker, the
/// comment syntax, and the `ninfer-serve` program name the dialect's launcher
/// scripts use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    /// A Windows batch file: `^` continuation, `rem` / `::` comments,
    /// `ninfer-serve.exe`.
    Batch,
    /// A POSIX shell script: `\` continuation, `#` comments, `ninfer-serve`.
    Shell,
}

impl Dialect {
    /// The line-continuation marker at the end of a physical line.
    fn continuation(self) -> char {
        match self {
            Dialect::Batch => '^',
            Dialect::Shell => '\\',
        }
    }

    /// The `ninfer-serve` program name the dialect's launcher scripts use.
    fn program_name(self) -> &'static str {
        match self {
            Dialect::Batch => "ninfer-serve.exe",
            Dialect::Shell => "ninfer-serve",
        }
    }

    /// Whether `line` is a comment in the dialect: a `rem` line (the first
    /// word, case-insensitive) or a `::` line in batch, a `#` line in shell.
    fn is_comment(self, line: &str) -> bool {
        let t = line.trim_start();
        match self {
            Dialect::Batch => {
                t.starts_with("::")
                    || t.split_whitespace()
                        .next()
                        .is_some_and(|w| w.eq_ignore_ascii_case("rem"))
            }
            Dialect::Shell => t.starts_with('#'),
        }
    }
}

/// A configuration parsed from a launcher script (v0.3): the name (the
/// script file name without the extension) and the command line (the
/// arguments after the `ninfer-serve` program token, with the whitespace
/// runs collapsed to single spaces).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedConfig {
    pub name: String,
    pub command_line: String,
}

/// The dialect of the launcher scripts on this platform (v0.3): batch on
/// Windows, shell on the other platforms.
pub fn platform_dialect() -> Dialect {
    #[cfg(windows)]
    {
        Dialect::Batch
    }
    #[cfg(not(windows))]
    {
        Dialect::Shell
    }
}

/// The launcher script extensions scanned for the dialect (v0.3): `.bat` for
/// batch, `.sh` for shell.
fn launcher_extensions(dialect: Dialect) -> &'static [&'static str] {
    match dialect {
        Dialect::Batch => &["bat"],
        Dialect::Shell => &["sh"],
    }
}

/// The logical lines of a launcher script: the physical lines joined across
/// the dialect's continuation marker (a trailing marker is dropped and the
/// lines are joined with a space).
fn logical_lines(content: &str, dialect: Dialect) -> Vec<String> {
    let continuation = dialect.continuation();
    let mut lines = Vec::new();
    let mut current = String::new();
    for raw in content.lines() {
        let trimmed = raw.trim_end();
        if trimmed.ends_with(continuation) {
            current.push_str(trimmed.trim_end_matches(continuation));
            current.push(' ');
        } else {
            current.push_str(trimmed);
            lines.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

/// Whether `token` names the `ninfer-serve` program of `dialect`: the file
/// name (the last path component, after stripping surrounding quotes) equals
/// the dialect's program name.
fn is_program_token(token: &str, dialect: Dialect) -> bool {
    let t = token.trim_matches('"');
    let name = t.split(['\\', '/']).next_back().unwrap_or(t);
    name == dialect.program_name()
}

/// Parses a launcher script in `dialect` into the `ninfer-serve` command
/// line (v0.3): the first logical line that invokes `ninfer-serve` directly,
/// with the program token removed and the whitespace runs collapsed to
/// single spaces. `None` when the script does not invoke `ninfer-serve`
/// directly, or the invocation carries no arguments.
pub fn parse_script(content: &str, dialect: Dialect) -> Option<String> {
    for line in logical_lines(content, dialect) {
        let trimmed = line.trim();
        if trimmed.is_empty() || dialect.is_comment(trimmed) {
            continue;
        }
        let first = trimmed.split_whitespace().next()?;
        if is_program_token(first, dialect) {
            let command_line = trimmed[first.len()..].trim();
            if !command_line.is_empty() {
                return Some(
                    command_line
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" "),
                );
            }
        }
    }
    None
}

/// Parses a launcher script in the platform dialect (v0.3) into the
/// `ninfer-serve` command line.
pub fn parse_launcher_script(content: &str) -> Option<String> {
    parse_script(content, platform_dialect())
}

/// Scans `folder` for the platform's launcher scripts (v0.3) and returns the
/// configurations parsed from them: one per script that invokes
/// `ninfer-serve` directly, named after the file name (without the
/// extension), sorted by name. Scripts whose name (case-insensitive)
/// collides with an entry of `existing` or with an earlier script are
/// skipped; an unreadable folder or script is skipped silently.
pub fn scan_folder(folder: &Path, existing: &[String]) -> Vec<ImportedConfig> {
    let dialect = platform_dialect();
    let mut taken: HashSet<String> = existing
        .iter()
        .map(|n| n.trim().to_ascii_lowercase())
        .collect();
    let mut scripts: Vec<(String, PathBuf)> = match std::fs::read_dir(folder) {
        Ok(entries) => entries
            .flatten()
            .filter_map(|entry| {
                let path = entry.path();
                let extension = path.extension()?.to_str()?.to_ascii_lowercase();
                if launcher_extensions(dialect).contains(&extension.as_str()) && path.is_file() {
                    let name = path.file_stem()?.to_str()?.to_owned();
                    Some((name, path))
                } else {
                    None
                }
            })
            .collect(),
        Err(_) => return Vec::new(),
    };
    scripts.sort_by(|a, b| a.0.cmp(&b.0));
    let mut result = Vec::new();
    for (name, path) in scripts {
        let key = name.to_ascii_lowercase();
        if !taken.insert(key) {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Some(command_line) = parse_script(&content, dialect) else {
            continue;
        };
        result.push(ImportedConfig { name, command_line });
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    const SINGLE_LINE_BAT: &str = r#"@echo off
.\ninfer-serve.exe models\qwen3_6_27b.ninfer --model-id qwen3.6-27b --max-context 150000 --default-max-tokens 150000 --spec mtp --draft-tokens 3 --lm-head-draft --host 0.0.0.0 --port 8080 --cors --preserve-thinking --webui --max-pending-requests 50 --pending-timeout-ms 3000000
"#;

    const MULTI_LINE_BAT: &str = r#"@echo off

REM .\ninfer-serve.exe models\qwen3_8_27b_nvfp4.ninfer --model-id qwen3.8-27b-nvfp4 --max-context 150000

REM
REM  --spec mtp --draft-tokens 3 --lm-head-draft ^

.\ninfer-serve.exe models\qwen3_8_27b_nvfp4.ninfer ^
  --model-id qwen3.8-27b-nvfp4 ^
  --max-context 200000 ^
  --kv-capacity 200000 ^
  --kv-dtype nvfp4 ^
  --max-concurrency 4 ^
  --device-state-slots 4 ^
  --host-state-slots 8 ^
  --host-kv-mib 16384 ^
  --default-max-tokens 32768 ^
  --prefill-chunk 1024 ^
  --spec dflash2 --draft-tokens 7 --lm-head-draft ^
  --preserve-thinking ^
  --host 0.0.0.0 --port 8080  --pending-timeout-ms 3000000 ^
  --request-log-jsonl "d:/NInfer/logs/server.requests.jsonl"
"#;

    const NO_INVOCATION_BAT: &str = r#"@echo off
rem  A helper script that does not launch ninfer-serve.
setlocal EnableExtensions
set "ROOT=%~dp0"
echo   Start it with the matching launcher (qwen3_*.bat) or directly:
echo       ninfer-serve.exe  models\%FILE%
pause
exit /b 0
"#;

    #[test]
    fn parse_single_line_bat() {
        let command_line = parse_script(SINGLE_LINE_BAT, Dialect::Batch).unwrap();
        assert_eq!(
            command_line,
            "models\\qwen3_6_27b.ninfer --model-id qwen3.6-27b --max-context 150000 --default-max-tokens 150000 --spec mtp --draft-tokens 3 --lm-head-draft --host 0.0.0.0 --port 8080 --cors --preserve-thinking --webui --max-pending-requests 50 --pending-timeout-ms 3000000"
        );
    }

    #[test]
    fn parse_multi_line_bat_with_continuations_and_comments() {
        let command_line = parse_script(MULTI_LINE_BAT, Dialect::Batch).unwrap();
        assert_eq!(
            command_line,
            "models\\qwen3_8_27b_nvfp4.ninfer --model-id qwen3.8-27b-nvfp4 --max-context 200000 --kv-capacity 200000 --kv-dtype nvfp4 --max-concurrency 4 --device-state-slots 4 --host-state-slots 8 --host-kv-mib 16384 --default-max-tokens 32768 --prefill-chunk 1024 --spec dflash2 --draft-tokens 7 --lm-head-draft --preserve-thinking --host 0.0.0.0 --port 8080 --pending-timeout-ms 3000000 --request-log-jsonl \"d:/NInfer/logs/server.requests.jsonl\""
        );
    }

    #[test]
    fn parse_bat_without_invocation_is_none() {
        assert_eq!(parse_script(NO_INVOCATION_BAT, Dialect::Batch), None);
    }

    #[test]
    fn parse_bat_commented_out_invocation_is_none() {
        let content = "REM .\\ninfer-serve.exe --model-id x\n";
        assert_eq!(parse_script(content, Dialect::Batch), None);
    }

    #[test]
    fn parse_bat_bare_invocation_is_none() {
        let content = ".\\ninfer-serve.exe\n";
        assert_eq!(parse_script(content, Dialect::Batch), None);
    }

    #[test]
    fn parse_bat_quoted_program_path() {
        let content = "\"C:\\NInfer\\ninfer-serve.exe\" --model-id x --port 8080\n";
        assert_eq!(
            parse_script(content, Dialect::Batch).as_deref(),
            Some("--model-id x --port 8080")
        );
    }

    #[test]
    fn parse_bat_ignores_indirect_invocation() {
        let content = "call ninfer-serve.exe --model-id x\n";
        assert_eq!(parse_script(content, Dialect::Batch), None);
    }

    const SINGLE_LINE_SH: &str = "#!/bin/sh\n# Launch the Qwen3.8-27B server.\n./ninfer-serve models/qwen3_8_27b.ninfer --model-id qwen3.8-27b --port 8080\n";

    const MULTI_LINE_SH: &str = "#!/bin/sh
# Launch the Qwen3.8-27B NVFP4 server.
./ninfer-serve models/qwen3_8_27b_nvfp4.ninfer \
  --model-id qwen3.8-27b-nvfp4 \
  --max-context 200000 \
  --request-log-jsonl /var/log/ninfer/server.requests.jsonl
";

    #[test]
    fn parse_single_line_sh() {
        let command_line = parse_script(SINGLE_LINE_SH, Dialect::Shell).unwrap();
        assert_eq!(
            command_line,
            "models/qwen3_8_27b.ninfer --model-id qwen3.8-27b --port 8080"
        );
    }

    #[test]
    fn parse_multi_line_sh_with_continuations_and_comments() {
        let command_line = parse_script(MULTI_LINE_SH, Dialect::Shell).unwrap();
        assert_eq!(
            command_line,
            "models/qwen3_8_27b_nvfp4.ninfer --model-id qwen3.8-27b-nvfp4 --max-context 200000 --request-log-jsonl /var/log/ninfer/server.requests.jsonl"
        );
    }

    #[test]
    fn parse_sh_without_invocation_is_none() {
        let content = "#!/bin/sh\n# A helper script.\necho hello\n";
        assert_eq!(parse_script(content, Dialect::Shell), None);
    }

    #[test]
    fn parse_sh_bare_invocation_is_none() {
        let content = "./ninfer-serve\n";
        assert_eq!(parse_script(content, Dialect::Shell), None);
    }

    #[test]
    fn parse_sh_absolute_program_path() {
        let content = "/opt/ninfer/ninfer-serve --model-id x\n";
        assert_eq!(
            parse_script(content, Dialect::Shell).as_deref(),
            Some("--model-id x")
        );
    }

    /// The launcher script content for `name` in the platform dialect.
    fn platform_script(name: &str) -> (String, String) {
        match platform_dialect() {
            Dialect::Batch => (
                format!("{name}.bat"),
                format!(".\\ninfer-serve.exe --model-id {name} --port 8080\n"),
            ),
            Dialect::Shell => (
                format!("{name}.sh"),
                format!("./ninfer-serve --model-id {name} --port 8080\n"),
            ),
        }
    }

    #[test]
    fn scan_folder_parsers_launcher_scripts() {
        let dir = tempfile::tempdir().unwrap();
        let (file_a, script_a) = platform_script("a");
        let (file_b, script_b) = platform_script("b");
        let dialect = platform_dialect();
        let multi = match dialect {
            Dialect::Batch => "@echo off\n.\\ninfer-serve.exe --model-id c ^\n  --port 8082\n",
            Dialect::Shell => "#!/bin/sh\n./ninfer-serve --model-id c \\\n  --port 8082\n",
        };
        let skip = match dialect {
            Dialect::Batch => "echo not a launcher\n",
            Dialect::Shell => "# not a launcher\necho hello\n",
        };
        std::fs::write(dir.path().join(&file_b), script_b).unwrap();
        std::fs::write(dir.path().join(&file_a), script_a).unwrap();
        std::fs::write(dir.path().join("c.bat"), multi).unwrap();
        std::fs::write(dir.path().join("c.sh"), multi).unwrap();
        std::fs::write(dir.path().join("skip.bat"), skip).unwrap();
        std::fs::write(dir.path().join("skip.sh"), skip).unwrap();
        std::fs::write(dir.path().join("notes.txt"), "ignored\n").unwrap();
        let configs = scan_folder(dir.path(), &[]);
        let names: Vec<&str> = configs.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["a", "b", "c"]);
        assert_eq!(configs[0].command_line, "--model-id a --port 8080");
        assert_eq!(configs[1].command_line, "--model-id b --port 8080");
        assert_eq!(configs[2].command_line, "--model-id c --port 8082");
    }

    #[test]
    fn scan_folder_skips_existing_names_case_insensitively() {
        let dir = tempfile::tempdir().unwrap();
        let (file_a, script_a) = platform_script("a");
        let (file_b, script_b) = platform_script("b");
        std::fs::write(dir.path().join(&file_a), script_a).unwrap();
        std::fs::write(dir.path().join(&file_b), script_b).unwrap();
        let configs = scan_folder(dir.path(), &["A".to_owned()]);
        let names: Vec<&str> = configs.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["b"]);
    }

    #[test]
    fn scan_folder_missing_folder_is_empty() {
        let missing = std::env::temp_dir().join("ninfer-monitor-test-missing-folder");
        assert!(scan_folder(&missing, &[]).is_empty());
    }
}
