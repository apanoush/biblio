//! `biblio` gathers bibliographic metadata from a filesystem tree of papers
//! and aggregates it into a local SQLite database.
//!
//! The pipeline is:
//! [`discover`] -> [`classify`] -> [`metadata`] / [`filename`] -> [`merge`] -> [`db`].
//!
//! See `doc/index.md` and `doc/architecture.md` for the design rationale.

pub mod classify;
pub mod config;
pub mod db;
pub mod discover;
pub mod filename;
pub mod merge;
pub mod metadata;
pub mod model;
pub mod report;
pub mod stats;
pub mod sync;
