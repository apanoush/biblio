//! Extract embedded document metadata by shelling out to `exiftool`.
//!
//! `exiftool` is chosen over a pure-Rust parser because it reads both the PDF
//! Info dictionary and XMP, and normalises the many ways publishers encode
//! DOI, authors and keywords. Files are passed in batches to amortise process
//! startup cost.

use std::path::Path;

use anyhow::{Context, Result};
use serde_json::Value;

/// Embedded metadata recovered from a single document.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Metadata {
    pub title: Option<String>,
    pub authors: Vec<String>,
    pub doi: Option<String>,
    pub publisher: Option<String>,
    pub keywords: Vec<String>,
    pub page_count: Option<i64>,
    pub producer: Option<String>,
    /// The raw exiftool JSON object, kept for auditing.
    pub raw: Option<String>,
}

/// Extract metadata for a batch of files.
///
/// The returned vector is aligned index-for-index with `files`. A file for
/// which exiftool reports an error yields a default [`Metadata`].
pub fn extract_batch(files: &[&Path]) -> Result<Vec<Metadata>> {
    if files.is_empty() {
        return Ok(Vec::new());
    }

    let mut cmd = std::process::Command::new("exiftool");
    cmd.args([
        "-j",
        "-charset",
        "utf8",
        "-Title",
        "-Author",
        "-Creator",
        "-DOI",
        "-Publisher",
        "-Subject",
        "-Keywords",
        "-PageCount",
        "-Producer",
    ]);
    for f in files {
        cmd.arg(f);
    }

    let output = cmd
        .output()
        .context("running exiftool (is it installed and on PATH?)")?;
    // exiftool exits non-zero if any file had an error but still prints JSON.
    let stdout = String::from_utf8_lossy(&output.stdout);
    if stdout.trim().is_empty() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("exiftool produced no output: {}", stderr.trim());
    }

    let parsed: Value = serde_json::from_str(&stdout).context("parsing exiftool JSON output")?;
    let array = parsed
        .as_array()
        .context("expected exiftool JSON to be an array")?;

    let mut result = Vec::with_capacity(files.len());
    for value in array {
        result.push(parse_one(value));
    }
    Ok(result)
}

fn parse_one(value: &Value) -> Metadata {
    if value.get("Error").is_some() {
        return Metadata {
            raw: serde_json::to_string(value).ok(),
            ..Metadata::default()
        };
    }

    Metadata {
        title: get_str(value, &["Title", "PDF:Title", "XMP:Title"]),
        authors: get_authors(value),
        doi: get_doi(value),
        publisher: get_str(value, &["Publisher", "XMP:Publisher"]),
        keywords: get_list(value, &["Subject", "Keywords", "PDF:Keywords"]),
        page_count: value.get("PageCount").and_then(Value::as_i64),
        producer: get_str(value, &["Producer"]),
        raw: serde_json::to_string(value).ok(),
    }
}

/// Read the first non-empty string among `keys`, joining arrays with `; `.
fn get_str(value: &Value, keys: &[&str]) -> Option<String> {
    for key in keys {
        match value.get(*key) {
            Some(Value::String(s)) if !s.trim().is_empty() => {
                return Some(s.trim().to_string());
            }
            Some(Value::Array(items)) => {
                let joined = items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join("; ");
                if !joined.is_empty() {
                    return Some(joined);
                }
            }
            _ => {}
        }
    }
    None
}

/// Read a list-valued field, splitting string values on `;`.
fn get_list(value: &Value, keys: &[&str]) -> Vec<String> {
    for key in keys {
        match value.get(*key) {
            Some(Value::Array(items)) => {
                return items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect();
            }
            Some(Value::String(s)) if !s.trim().is_empty() => {
                return s
                    .split(';')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect();
            }
            _ => {}
        }
    }
    Vec::new()
}

fn get_authors(value: &Value) -> Vec<String> {
    let mut authors = split_names(get_str(value, &["Author"]));
    if authors.is_empty() {
        // Some tools (e.g. pikepdf) place the author list in `Creator`.
        if let Some(Value::Array(items)) = value.get("Creator") {
            let names: Vec<String> = items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect();
            if names.iter().all(|n| looks_like_name(n)) {
                authors = names;
            }
        }
    }
    authors
}

fn split_names(raw: Option<String>) -> Vec<String> {
    let Some(raw) = raw else {
        return Vec::new();
    };
    let sep = if raw.contains(';') { ';' } else { ',' };
    raw.split(sep)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Heuristic: a name has letters, no digits, and is not a known tool name.
fn looks_like_name(s: &str) -> bool {
    s.chars().any(char::is_alphabetic)
        && !s.chars().any(|c| c.is_ascii_digit())
        && !s.eq_ignore_ascii_case("latex")
        && !s.to_ascii_lowercase().contains("pdftex")
}

fn get_doi(value: &Value) -> Option<String> {
    let raw = get_str(value, &["DOI", "Doi", "XMP:DOI"])?;
    Some(normalize_doi(&raw))
}

/// Strip URL prefixes commonly found around DOI values.
pub fn normalize_doi(raw: &str) -> String {
    let s = raw.trim();
    for prefix in [
        "https://doi.org/",
        "http://doi.org/",
        "https://dx.doi.org/",
        "http://dx.doi.org/",
        "doi:",
    ] {
        if let Some(rest) = s.strip_prefix(prefix) {
            return rest.trim().to_string();
        }
    }
    s.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_strips_url_prefixes() {
        assert_eq!(normalize_doi("https://doi.org/10.1/x"), "10.1/x");
        assert_eq!(normalize_doi("doi:10.1/x"), "10.1/x");
        assert_eq!(normalize_doi("10.1/x"), "10.1/x");
    }

    #[test]
    fn parse_extracts_common_fields() {
        let v: Value = serde_json::json!({
            "Title": "A title",
            "Author": "A. One; B. Two",
            "DOI": "https://doi.org/10.1234/abc",
            "Subject": ["x", "y"],
            "PageCount": 12
        });
        let m = parse_one(&v);
        assert_eq!(m.title.as_deref(), Some("A title"));
        assert_eq!(m.authors, vec!["A. One", "B. Two"]);
        assert_eq!(m.doi.as_deref(), Some("10.1234/abc"));
        assert_eq!(m.keywords, vec!["x", "y"]);
        assert_eq!(m.page_count, Some(12));
    }
}
