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

/// Convert "yes/true/1" to bool
pub fn string_to_bool(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let upper = s.to_uppercase();
    matches!(upper.as_str(), "YES" | "TRUE" | "1" | "ON")
}

/// Extract values from a section, ignoring entry numbers
pub fn extract_values(entries: Vec<(u32, String)>) -> Vec<String> {
    entries.into_iter().map(|(_, v)| v).collect()
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
        assert_eq!(extract_values(unit["DESCRIPTION"].clone()), vec!["Test Service"]);
    }
    
    #[test]
    fn test_string_to_bool() {
        assert!(string_to_bool("yes"));
        assert!(string_to_bool("YES"));
        assert!(string_to_bool("true"));
        assert!(string_to_bool("1"));
        assert!(!string_to_bool("no"));
        assert!(!string_to_bool("0"));
        assert!(!string_to_bool(""));
    }
}
