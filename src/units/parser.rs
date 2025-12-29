//! INI-style unit file parser
//!
//! Parses systemd unit files into structured data.
//! Based on rustysd's parser with async/modernization.

use std::collections::HashMap;
use std::path::Path;

/// A section contains key-value pairs, where each key can have multiple values
/// The u32 is the order the value appeared (for stable ordering)
pub type ParsedSection = HashMap<String, Vec<(u32, String)>>;

/// A parsed unit file is a map of section names to their contents
pub type ParsedFile = HashMap<String, ParsedSection>;

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("Section '{0}' appears more than once")]
    DuplicateSection(String),
    
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    
    #[error("Unknown setting: {0}")]
    UnknownSetting(String),
    
    #[error("Setting '{0}' has too many values: {1:?}")]
    TooManyValues(String, Vec<String>),
    
    #[error("Unsupported setting: {0}")]
    UnsupportedSetting(String),
    
    #[error("Parse error: {0}")]
    Generic(String),
}

/// Parse a unit file from a string
pub fn parse_file(content: &str) -> Result<ParsedFile, ParseError> {
    let mut sections = HashMap::new();
    let lines: Vec<&str> = content.lines().map(|s| s.trim()).collect();
    
    let mut lines_iter = lines.iter().peekable();
    
    // Skip lines before the first section
    while lines_iter.peek().map_or(false, |l| !l.starts_with('[')) {
        lines_iter.next();
    }
    
    // Get first section name
    let Some(first_section) = lines_iter.next() else {
        return Ok(sections); // Empty file
    };
    
    let mut current_section_name = first_section.to_string();
    let mut current_section_lines = Vec::new();
    
    for line in lines_iter {
        if line.starts_with('[') {
            // New section - store current one
            if sections.contains_key(&current_section_name) {
                return Err(ParseError::DuplicateSection(current_section_name));
            }
            sections.insert(
                current_section_name.clone(),
                parse_section(&current_section_lines),
            );
            current_section_name = line.to_string();
            current_section_lines.clear();
        } else {
            current_section_lines.push(*line);
        }
    }
    
    // Insert last section
    if !current_section_name.is_empty() {
        if sections.contains_key(&current_section_name) {
            return Err(ParseError::DuplicateSection(current_section_name));
        }
        sections.insert(current_section_name, parse_section(&current_section_lines));
    }
    
    Ok(sections)
}

/// Keys that accept space-separated multiple values
const SPACE_SEPARATED_KEYS: &[&str] = &[
    "AFTER", "BEFORE", "REQUIRES", "WANTS", "CONFLICTS", "WANTEDBY", "REQUIREDBY",
    "CONDITIONPATHEXISTS",
];

/// Parse a single section's lines into key-value pairs
fn parse_section(lines: &[&str]) -> ParsedSection {
    let mut entries: ParsedSection = HashMap::new();
    let mut entry_number = 0u32;

    for line in lines {
        // Skip comments and empty lines
        if line.starts_with('#') || line.starts_with(';') || line.is_empty() {
            continue;
        }

        // Find the = separator
        let Some(pos) = line.find('=') else {
            continue;
        };

        let (name, value) = line.split_at(pos);
        let value = value.trim_start_matches('=').trim();
        let name = name.trim().to_uppercase();

        // Determine separator: space for dependency keys, comma otherwise
        let values: Vec<String> = if SPACE_SEPARATED_KEYS.contains(&name.as_str()) {
            // Split on whitespace for dependency keys
            value.split_whitespace().map(|s| s.to_string()).collect()
        } else {
            // Split on comma for other keys (like Environment=)
            value.split(',').map(|x| x.trim().to_string()).collect()
        };

        let vec = entries.entry(name).or_default();
        for v in values {
            if !v.is_empty() {
                vec.push((entry_number, v));
                entry_number += 1;
            }
        }
    }

    entries
}

/// Parse an async unit file from disk
pub async fn parse_unit_file(path: &Path) -> Result<ParsedFile, ParseError> {
    let content = tokio::fs::read_to_string(path).await?;
    parse_file(&content)
}

/// Parse Environment= values using shell-like quoting
pub fn parse_environment(raw: &str) -> Result<Vec<(String, String)>, ParseError> {
    let parts = shlex::split(raw).ok_or_else(|| {
        ParseError::Generic(format!("Invalid shell quoting in: {}", raw))
    })?;
    
    let mut vars = Vec::new();
    for pair in parts {
        if let Some((key, value)) = pair.split_once('=') {
            vars.push((key.to_string(), value.to_string()));
        }
    }
    Ok(vars)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Extract just the string values from parsed entries (for tests)
    fn extract_values(entries: Vec<(u32, String)>) -> Vec<String> {
        let mut sorted = entries;
        sorted.sort_by_key(|(order, _)| *order);
        sorted.into_iter().map(|(_, v)| v).collect()
    }

    /// Parse a boolean string value (for tests)
    fn string_to_bool(s: &str) -> bool {
        matches!(s.to_lowercase().as_str(), "yes" | "true" | "1" | "on")
    }

    #[test]
    fn test_parse_simple_service() {
        let content = r#"
[Unit]
Description=Test Service
After=network.target

[Service]
Type=simple
ExecStart=/usr/bin/test

[Install]
WantedBy=multi-user.target
"#;
        let parsed = parse_file(content).unwrap();

        assert!(parsed.contains_key("[Unit]"));
        assert!(parsed.contains_key("[Service]"));
        assert!(parsed.contains_key("[Install]"));

        let unit = &parsed["[Unit]"];
        assert_eq!(
            extract_values(unit["DESCRIPTION"].clone()),
            vec!["Test Service"]
        );
    }

    #[test]
    fn test_string_to_bool() {
        assert!(string_to_bool("yes"));
        assert!(string_to_bool("YES"));
        assert!(string_to_bool("true"));
        assert!(string_to_bool("1"));
        assert!(string_to_bool("on"));
        assert!(!string_to_bool("no"));
        assert!(!string_to_bool("false"));
        assert!(!string_to_bool("0"));
        assert!(!string_to_bool("off"));
        assert!(!string_to_bool(""));
    }

    #[test]
    fn test_empty_file() {
        let parsed = parse_file("").unwrap();
        assert!(parsed.is_empty());
    }

    #[test]
    fn test_comments_only() {
        let content = "# This is a comment\n; Another comment\n";
        let parsed = parse_file(content).unwrap();
        assert!(parsed.is_empty());
    }

    #[test]
    fn test_hash_comments() {
        let content = r#"
[Unit]
# This is a comment
Description=Test
# Another comment
After=network.target
"#;
        let parsed = parse_file(content).unwrap();
        let unit = &parsed["[Unit]"];
        assert_eq!(unit.len(), 2); // Description and After
    }

    #[test]
    fn test_semicolon_comments() {
        let content = r#"
[Unit]
; This is a comment
Description=Test
"#;
        let parsed = parse_file(content).unwrap();
        let unit = &parsed["[Unit]"];
        assert_eq!(
            extract_values(unit["DESCRIPTION"].clone()),
            vec!["Test"]
        );
    }

    #[test]
    fn test_space_separated_dependencies() {
        let content = r#"
[Unit]
After=a.target b.target c.target
Wants=x.service y.service
"#;
        let parsed = parse_file(content).unwrap();
        let unit = &parsed["[Unit]"];

        let after = extract_values(unit["AFTER"].clone());
        assert_eq!(after, vec!["a.target", "b.target", "c.target"]);

        let wants = extract_values(unit["WANTS"].clone());
        assert_eq!(wants, vec!["x.service", "y.service"]);
    }

    #[test]
    fn test_repeated_keys() {
        let content = r#"
[Service]
ExecStartPre=/bin/echo one
ExecStartPre=/bin/echo two
ExecStartPre=/bin/echo three
"#;
        let parsed = parse_file(content).unwrap();
        let service = &parsed["[Service]"];

        let pre = extract_values(service["EXECSTARTPRE"].clone());
        assert_eq!(
            pre,
            vec!["/bin/echo one", "/bin/echo two", "/bin/echo three"]
        );
    }

    #[test]
    fn test_empty_value() {
        let content = r#"
[Service]
ExecStart=
"#;
        let parsed = parse_file(content).unwrap();
        let service = &parsed["[Service]"];
        // Empty value should result in empty vec
        assert!(service.get("EXECSTART").map_or(true, |v| v.is_empty()));
    }

    #[test]
    fn test_value_with_equals() {
        let content = r#"
[Service]
Environment=FOO=bar=baz
"#;
        let parsed = parse_file(content).unwrap();
        let service = &parsed["[Service]"];
        let env = extract_values(service["ENVIRONMENT"].clone());
        assert_eq!(env, vec!["FOO=bar=baz"]);
    }

    #[test]
    fn test_key_case_insensitive() {
        let content = r#"
[Unit]
description=Lower
DESCRIPTION=Upper
Description=Mixed
"#;
        let parsed = parse_file(content).unwrap();
        let unit = &parsed["[Unit]"];
        // All should be normalized to uppercase and combined
        let desc = extract_values(unit["DESCRIPTION"].clone());
        assert_eq!(desc.len(), 3);
    }

    #[test]
    fn test_whitespace_handling() {
        let content = r#"
[Unit]
   Description   =   Test Service
After    =    network.target
"#;
        let parsed = parse_file(content).unwrap();
        let unit = &parsed["[Unit]"];
        assert_eq!(
            extract_values(unit["DESCRIPTION"].clone()),
            vec!["Test Service"]
        );
    }

    #[test]
    fn test_duplicate_section_error() {
        let content = r#"
[Unit]
Description=First

[Unit]
Description=Second
"#;
        let result = parse_file(content);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ParseError::DuplicateSection(_)));
    }

    #[test]
    fn test_lines_before_first_section() {
        let content = r#"
# Header comment
; Another comment

[Unit]
Description=Test
"#;
        let parsed = parse_file(content).unwrap();
        assert!(parsed.contains_key("[Unit]"));
    }

    #[test]
    fn test_parse_environment() {
        let result = parse_environment("FOO=bar BAZ=qux").unwrap();
        assert_eq!(result, vec![("FOO".into(), "bar".into()), ("BAZ".into(), "qux".into())]);
    }

    #[test]
    fn test_parse_environment_quoted() {
        let result = parse_environment(r#"FOO="bar baz" QUX=test"#).unwrap();
        assert_eq!(result, vec![("FOO".into(), "bar baz".into()), ("QUX".into(), "test".into())]);
    }

    #[test]
    fn test_special_characters_in_value() {
        let content = r#"
[Service]
ExecStart=/usr/bin/test --flag="value with spaces" -x
"#;
        let parsed = parse_file(content).unwrap();
        let service = &parsed["[Service]"];
        let exec = extract_values(service["EXECSTART"].clone());
        assert_eq!(exec, vec![r#"/usr/bin/test --flag="value with spaces" -x"#]);
    }

    #[test]
    fn test_percent_specifiers_preserved() {
        let content = r#"
[Service]
ExecStart=/usr/bin/test %n %i %h
"#;
        let parsed = parse_file(content).unwrap();
        let service = &parsed["[Service]"];
        let exec = extract_values(service["EXECSTART"].clone());
        assert_eq!(exec, vec!["/usr/bin/test %n %i %h"]);
    }

    #[test]
    fn test_dollar_variables_preserved() {
        let content = r#"
[Service]
ExecStart=/bin/sh -c "echo $HOME"
ExecReload=/bin/kill -HUP $MAINPID
"#;
        let parsed = parse_file(content).unwrap();
        let service = &parsed["[Service]"];
        let reload = extract_values(service["EXECRELOAD"].clone());
        assert_eq!(reload, vec!["/bin/kill -HUP $MAINPID"]);
    }
}
