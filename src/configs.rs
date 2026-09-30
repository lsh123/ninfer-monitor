// NInferMonitor
//
// This is free software; see the LICENSE.md file in the source distribution for precise wording.
//
// Copyright (C) 2026 Aleksey Sanin aleksey@aleksey.com. All Rights Reserved.

/// The default name for a new configuration (v0.3 M3): the smallest unused
/// `Config N`, compared case-insensitively against the trimmed existing
/// names.
pub fn next_config_name(existing: &[String]) -> String {
    let taken: std::collections::HashSet<String> = existing
        .iter()
        .map(|name| name.trim().to_lowercase())
        .collect();
    let mut n = 1u32;
    loop {
        let candidate = format!("Config {n}");
        if !taken.contains(&candidate.to_lowercase()) {
            return candidate;
        }
        n += 1;
    }
}

/// Validate a configuration name (v0.3 M3): it must be non-empty after
/// trimming and unique (case-insensitive) among the other configuration
/// names. `others` is the set of names to compare against (the other
/// configurations, excluding the one being edited).
pub fn validate_name(name: &str, others: &[String]) -> Result<(), String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("Error: the name is empty".to_owned());
    }
    let lower = trimmed.to_lowercase();
    if others
        .iter()
        .any(|other| other.trim().to_lowercase() == lower)
    {
        return Err("Error: the name must be unique".to_owned());
    }
    Ok(())
}

/// Validate a configuration command line (v0.3 M3): it must be non-empty
/// after trimming.
pub fn validate_command_line(line: &str) -> Result<(), String> {
    if line.trim().is_empty() {
        return Err("Error: the command line is empty".to_owned());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_name_starts_at_one() {
        assert_eq!(next_config_name(&[]), "Config 1");
    }

    #[test]
    fn next_name_increments() {
        let existing = vec!["Config 1".to_owned(), "Config 2".to_owned()];
        assert_eq!(next_config_name(&existing), "Config 3");
    }

    #[test]
    fn next_name_fills_gaps() {
        // Config 2 is missing, so the smallest unused number is 2.
        let existing = vec!["Config 1".to_owned(), "Config 3".to_owned()];
        assert_eq!(next_config_name(&existing), "Config 2");
    }

    #[test]
    fn next_name_is_case_insensitive() {
        let existing = vec!["config 1".to_owned(), "CONFIG 2".to_owned()];
        assert_eq!(next_config_name(&existing), "Config 3");
    }

    #[test]
    fn next_name_ignores_surrounding_whitespace() {
        let existing = vec!["  Config 1  ".to_owned()];
        assert_eq!(next_config_name(&existing), "Config 2");
    }

    #[test]
    fn next_name_ignores_non_default_names() {
        let existing = vec!["my-server".to_owned(), "bench-run".to_owned()];
        assert_eq!(next_config_name(&existing), "Config 1");
    }

    #[test]
    fn validate_name_accepts_a_valid_name() {
        let others = vec!["a".to_owned(), "b".to_owned()];
        assert!(validate_name("c", &others).is_ok());
    }

    #[test]
    fn validate_name_rejects_empty_and_blank() {
        assert_eq!(
            validate_name("", &[]).unwrap_err(),
            "Error: the name is empty"
        );
        assert_eq!(
            validate_name("   ", &[]).unwrap_err(),
            "Error: the name is empty"
        );
    }

    #[test]
    fn validate_name_rejects_duplicate_case_insensitively() {
        let others = vec!["My-Server".to_owned()];
        assert_eq!(
            validate_name("my-server", &others).unwrap_err(),
            "Error: the name must be unique"
        );
        assert_eq!(
            validate_name("MY-SERVER", &others).unwrap_err(),
            "Error: the name must be unique"
        );
    }

    #[test]
    fn validate_name_trims_before_comparing() {
        let others = vec!["  a  ".to_owned()];
        assert_eq!(
            validate_name("a", &others).unwrap_err(),
            "Error: the name must be unique"
        );
    }

    #[test]
    fn validate_command_line_accepts_non_empty() {
        assert!(validate_command_line("--model x").is_ok());
    }

    #[test]
    fn validate_command_line_rejects_empty_and_blank() {
        assert_eq!(
            validate_command_line("").unwrap_err(),
            "Error: the command line is empty"
        );
        assert_eq!(
            validate_command_line("   \t ").unwrap_err(),
            "Error: the command line is empty"
        );
    }
}
