//! Core data structures shared across the pipeline.
//!
//! The design is data-oriented: plain structs describing a paper, an author
//! and the provenance of each extracted field. Functions in the other modules
//! operate on these structures.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Where the value of a field came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FieldSource {
    /// Embedded document metadata (e.g. PDF Info dict / XMP via exiftool).
    Embedded,
    /// Parsed from the file name.
    Filename,
    /// Inferred from the directory structure.
    Path,
    /// Computed from another field (e.g. arXiv DOI from an arXiv id).
    Derived,
}

impl FieldSource {
    /// Stable lowercase identifier used in the database.
    pub fn as_str(self) -> &'static str {
        match self {
            FieldSource::Embedded => "embedded",
            FieldSource::Filename => "filename",
            FieldSource::Path => "path",
            FieldSource::Derived => "derived",
        }
    }
}

impl fmt::Display for FieldSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A single author, with its ordering within the paper.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Author {
    /// Zero-based position in the author list.
    pub position: u32,
    /// Full author name as extracted.
    pub name: String,
}

/// Records where one field of a paper was obtained from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Provenance {
    /// Field name, matching a column in the `papers` table.
    pub field: String,
    /// Where the value came from.
    pub source: FieldSource,
    /// Confidence in `[0.0, 1.0]`.
    pub confidence: f64,
}

/// A fully extracted paper, ready to be persisted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Paper {
    /// Path relative to the papers root; the stable primary key.
    pub path: String,
    /// Absolute path at scan time.
    pub abs_path: String,
    /// Top-level category folder (journal, conference, ...).
    pub category: String,
    /// Optional second-level folder (e.g. `chuv`), `None` for `pdfs`.
    pub subcategory: Option<String>,
    /// File extension, lowercase (pdf, html, ...).
    pub filetype: String,
    pub title: Option<String>,
    pub authors: Vec<Author>,
    pub publisher: Option<String>,
    pub doi: Option<String>,
    pub arxiv_id: Option<String>,
    pub arxiv_doi: Option<String>,
    pub venue: Option<String>,
    pub year: Option<i32>,
    pub page_count: Option<i64>,
    pub keywords: Vec<String>,
    /// File size in bytes.
    pub size: i64,
    /// Modification time, nanoseconds since the Unix epoch.
    pub mtime_ns: i64,
    /// Optional content hash (only when enabled in config).
    pub sha256: Option<String>,
}
