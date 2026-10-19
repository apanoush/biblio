//! Scan statistics and verbose reporting.
//!
//! The one-line end-of-run summary is formatted by the CLI (it needs styling);
//! this module owns the counters and the optional detail report.

use std::collections::BTreeMap;

use rusqlite::Connection;

use crate::db;

/// Summary of one `sync` run.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ScanStats {
    /// Files found on disk matching the configured extensions.
    pub discovered: usize,
    /// New rows inserted.
    pub inserted: usize,
    /// Existing rows refreshed.
    pub updated: usize,
    /// Files found on disk whose stored row was already current.
    pub unchanged: usize,
    /// Rows removed by `--prune` because the file is gone.
    pub deleted: usize,
    /// Files that could not be stored.
    pub errors: usize,
    /// Discovered files per top-level category.
    pub by_category: BTreeMap<String, usize>,
}

impl ScanStats {
    /// Record one discovered file under its category.
    pub fn observe(&mut self, category: &str) {
        self.discovered += 1;
        *self.by_category.entry(category.to_string()).or_insert(0) += 1;
    }

    /// Total number of rows written (inserted + updated), i.e. imported.
    pub fn imported(&self) -> usize {
        self.inserted + self.updated
    }
}

/// Print the verbose detail report: per-category counts and field coverage.
pub fn print_details(stats: &ScanStats, conn: &Connection) -> anyhow::Result<()> {
    if !stats.by_category.is_empty() {
        println!();
        println!("Discovered by category");
        for (category, count) in &stats.by_category {
            println!("  {category:<12} {count}");
        }
    }

    let total = db::count(conn)?.max(1);
    println!();
    println!("Field coverage (database)");
    for (field, n) in db::coverage(conn)? {
        println!(
            "  {field:<12} {n:>5}  ({:.0}%)",
            100.0 * n as f64 / total as f64
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imported_is_inserted_plus_updated() {
        let stats = ScanStats {
            inserted: 3,
            updated: 4,
            ..ScanStats::default()
        };
        assert_eq!(stats.imported(), 7);
    }

    #[test]
    fn observe_counts_discovered_and_categories() {
        let mut stats = ScanStats::default();
        stats.observe("journal");
        stats.observe("journal");
        stats.observe("preprints");
        assert_eq!(stats.discovered, 3);
        assert_eq!(stats.by_category["journal"], 2);
        assert_eq!(stats.by_category["preprints"], 1);
    }
}
