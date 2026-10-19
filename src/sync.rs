//! Scan orchestration: discover -> extract (parallel) -> store -> prune.
//!
//! This is domain logic, kept out of the CLI. Progress reporting is abstracted
//! behind the [`Progress`] trait so the library carries no terminal
//! dependency.

use std::collections::HashSet;
use std::path::Path;

use anyhow::{Context, Result};
use rayon::prelude::*;
use rusqlite::Connection;

use crate::config::Config;
use crate::db;
use crate::discover;
use crate::filename;
use crate::merge;
use crate::metadata::{self, Metadata};
use crate::report::ScanStats;

/// A progress sink for long-running phases.
pub trait Progress: Send + Sync {
    /// Begin a phase whose total is not yet known; show a spinner.
    fn start_spinner(&self, message: &str);
    /// Begin a phase whose total is known; show a bar.
    fn start_bar(&self, message: &str, total: u64);
    /// Advance the current phase by `n` units.
    fn inc(&self, n: u64);
    /// End and clear the current phase.
    fn finish(&self);
}

/// A [`Progress`] implementation that does nothing.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoProgress;

impl Progress for NoProgress {
    fn start_spinner(&self, _message: &str) {}
    fn start_bar(&self, _message: &str, _total: u64) {}
    fn inc(&self, _n: u64) {}
    fn finish(&self) {}
}

/// Options controlling a sync run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncOptions {
    /// Re-extract every file, ignoring stored fingerprints.
    pub force: bool,
    /// Remove rows whose file no longer exists.
    pub prune: bool,
    /// Compute a SHA-256 content hash per file.
    pub hash_contents: bool,
    /// Files per exiftool invocation.
    pub batch_size: usize,
}

impl Default for SyncOptions {
    fn default() -> Self {
        Self {
            force: false,
            prune: false,
            hash_contents: false,
            batch_size: 100,
        }
    }
}

/// Bring the database in sync with the papers tree.
///
/// Discovery and extraction are reported through `progress`. Extraction runs in
/// parallel; database writes are serialized on a single transaction.
pub fn sync(
    conn: &Connection,
    cfg: &Config,
    opts: &SyncOptions,
    progress: &dyn Progress,
) -> Result<ScanStats> {
    let mut stats = ScanStats::default();

    progress.start_spinner("discovering");
    let files = discover::discover(
        &cfg.papers_root,
        &cfg.include_extensions,
        opts.hash_contents,
    )?;
    progress.finish();

    let known = db::fingerprints(conn)?;
    let to_process: Vec<_> = files
        .iter()
        .filter(|f| {
            stats.observe(&f.classification.category);
            opts.force
                || known
                    .get(&f.rel_path)
                    .is_none_or(|k| k.size != f.size || k.mtime_ns != f.mtime_ns)
        })
        .collect();
    stats.unchanged = stats.discovered - to_process.len();

    let batch_size = opts.batch_size.max(1);
    progress.start_bar("extracting", to_process.len() as u64);
    let batches: Vec<Result<Vec<Metadata>, String>> = to_process
        .par_chunks(batch_size)
        .map(|chunk| {
            let paths: Vec<&Path> = chunk.iter().map(|f| f.abs_path.as_path()).collect();
            let result = metadata::extract_batch(&paths).map_err(|e| e.to_string());
            progress.inc(chunk.len() as u64);
            result
        })
        .collect();
    progress.finish();

    // Writes stay sequential, on one transaction, in path order.
    let tx = conn.unchecked_transaction()?;
    let mut warned = false;
    for (chunk, meta_result) in to_process.chunks(batch_size).zip(batches) {
        let (metas, available) = match meta_result {
            Ok(metas) => (metas, true),
            Err(err) => {
                if !warned {
                    tracing::warn!("embedded metadata unavailable: {err}");
                    warned = true;
                }
                (vec![Metadata::default(); chunk.len()], false)
            }
        };

        for (file, meta) in chunk.iter().zip(metas.iter()) {
            let stem = Path::new(&file.rel_path)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("");
            let parsed = filename::parse(stem);
            let embedded = available.then_some(meta);
            let extracted = merge::build(file, embedded, &parsed);

            if known.contains_key(&file.rel_path) {
                stats.updated += 1;
            } else {
                stats.inserted += 1;
            }

            if let Err(err) = db::upsert(&tx, &extracted) {
                tracing::error!("failed to store {}: {err}", file.rel_path);
                stats.errors += 1;
            }
        }
    }
    tx.commit().context("committing scan transaction")?;

    if opts.prune {
        let discovered: HashSet<&str> = files.iter().map(|f| f.rel_path.as_str()).collect();
        let stale: Vec<String> = db::paths(conn)?
            .into_iter()
            .filter(|path| !discovered.contains(path.as_str()))
            .collect();
        stats.deleted = db::delete_paths(conn, &stale)?;
    }

    Ok(stats)
}
