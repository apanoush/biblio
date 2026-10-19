//! Aggregate statistics over the catalog.
//!
//! Pure SQL domain logic, with no UI dependency. The CLI renders this as text
//! or JSON; the TUI renders the same structure interactively.

use anyhow::Result;
use rusqlite::Connection;
use serde::Serialize;

/// A labelled count (category, venue, author, ...).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Count {
    pub label: String,
    pub value: i64,
}

/// How many papers have a non-null value for a field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Coverage {
    pub field: String,
    pub present: i64,
    pub total: i64,
}

/// Number of provenance entries per (field, source).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProvenanceCount {
    pub field: String,
    pub source: String,
    pub count: i64,
}

/// A paper whose stored year is implausible.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct YearAnomaly {
    pub path: String,
    pub year: i32,
}

/// Data-quality problems to surface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Anomalies {
    pub implausible_years: Vec<YearAnomaly>,
    pub missing_title_doi: i64,
    pub duplicate_dois: Vec<Count>,
}

/// Everything the `stats` command and the TUI need.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Stats {
    pub total_papers: i64,
    pub distinct_authors: i64,
    pub distinct_dois: i64,
    pub distinct_arxiv: i64,
    pub total_bytes: i64,
    pub total_pages: i64,
    pub by_category: Vec<Count>,
    pub by_filetype: Vec<Count>,
    pub by_year: Vec<Count>,
    pub coverage: Vec<Coverage>,
    pub top_authors: Vec<Count>,
    pub top_venues: Vec<Count>,
    pub provenance: Vec<ProvenanceCount>,
    pub growth: Vec<Count>,
    pub anomalies: Anomalies,
}

/// Fields reported in the coverage section.
const COVERAGE_FIELDS: [&str; 8] = [
    "title",
    "authors",
    "doi",
    "arxiv_id",
    "publisher",
    "venue",
    "year",
    "page_count",
];

/// Collect all statistics, keeping at most `top_n` rows for the ranked lists.
pub fn collect(conn: &Connection, top_n: i64) -> Result<Stats> {
    let total_papers: i64 = conn.query_row("SELECT COUNT(*) FROM papers", [], |r| r.get(0))?;

    let total = |expr: &str| -> Result<i64> {
        let sql = format!("SELECT COALESCE({expr}, 0) FROM papers");
        Ok(conn.query_row(&sql, [], |r| r.get(0))?)
    };

    let distinct_authors: i64 =
        conn.query_row("SELECT COUNT(DISTINCT name) FROM authors", [], |r| r.get(0))?;

    let distinct_dois: i64 = conn.query_row(
        "SELECT COUNT(DISTINCT doi) FROM papers WHERE doi IS NOT NULL",
        [],
        |r| r.get(0),
    )?;
    let distinct_arxiv: i64 = conn.query_row(
        "SELECT COUNT(DISTINCT arxiv_id) FROM papers WHERE arxiv_id IS NOT NULL",
        [],
        |r| r.get(0),
    )?;

    let by_category = counts(
        conn,
        "SELECT category, COUNT(*) FROM papers GROUP BY category ORDER BY COUNT(*) DESC",
    )?;
    let by_filetype = counts(
        conn,
        "SELECT filetype, COUNT(*) FROM papers GROUP BY filetype ORDER BY COUNT(*) DESC",
    )?;

    // Plausible years only; out-of-range values are surfaced as anomalies.
    let by_year = counts(
        conn,
        "SELECT CAST(year AS TEXT), COUNT(*) FROM papers
         WHERE year >= 1900 AND year <= CAST(strftime('%Y','now') AS INTEGER) + 1
         GROUP BY year ORDER BY year",
    )?;

    let mut coverage = Vec::new();
    for field in COVERAGE_FIELDS {
        let present = if field == "authors" {
            conn.query_row("SELECT COUNT(DISTINCT paper_path) FROM authors", [], |r| {
                r.get(0)
            })?
        } else {
            conn.query_row(&format!("SELECT COUNT({field}) FROM papers"), [], |r| {
                r.get(0)
            })?
        };
        coverage.push(Coverage {
            field: field.to_string(),
            present,
            total: total_papers,
        });
    }

    let top_authors = counts(
        conn,
        &format!(
            "SELECT name, COUNT(*) FROM authors GROUP BY name
             ORDER BY COUNT(*) DESC, name LIMIT {top_n}"
        ),
    )?;
    let top_venues = counts(
        conn,
        &format!(
            "SELECT venue, COUNT(*) FROM papers WHERE venue IS NOT NULL
             GROUP BY venue ORDER BY COUNT(*) DESC, venue LIMIT {top_n}"
        ),
    )?;

    let provenance = {
        let mut stmt = conn.prepare(
            "SELECT field, source, COUNT(*) FROM provenance
             GROUP BY field, source ORDER BY field, COUNT(*) DESC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(ProvenanceCount {
                field: r.get(0)?,
                source: r.get(1)?,
                count: r.get(2)?,
            })
        })?;
        collect_rows(rows)?
    };

    let growth = counts(
        conn,
        "SELECT strftime('%Y-%m', first_seen), COUNT(*) FROM papers
         GROUP BY 1 ORDER BY 1",
    )?;

    let implausible_years = {
        let mut stmt = conn.prepare(
            "SELECT path, year FROM papers
             WHERE year < 1900 OR year > CAST(strftime('%Y','now') AS INTEGER) + 1
             ORDER BY year",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(YearAnomaly {
                path: r.get(0)?,
                year: r.get(1)?,
            })
        })?;
        collect_rows(rows)?
    };

    let missing_title_doi: i64 = conn.query_row(
        "SELECT COUNT(*) FROM papers WHERE title IS NULL AND doi IS NULL",
        [],
        |r| r.get(0),
    )?;

    let duplicate_dois = counts(
        conn,
        "SELECT doi, COUNT(*) FROM papers WHERE doi IS NOT NULL
         GROUP BY doi HAVING COUNT(*) > 1 ORDER BY COUNT(*) DESC",
    )?;

    Ok(Stats {
        total_papers,
        distinct_authors,
        distinct_dois,
        distinct_arxiv,
        total_bytes: total("SUM(size)")?,
        total_pages: total("SUM(page_count)")?,
        by_category,
        by_filetype,
        by_year,
        coverage,
        top_authors,
        top_venues,
        provenance,
        growth,
        anomalies: Anomalies {
            implausible_years,
            missing_title_doi,
            duplicate_dois,
        },
    })
}

/// Run a two-column `SELECT label, count` query into [`Count`] rows.
fn counts(conn: &Connection, sql: &str) -> Result<Vec<Count>> {
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map([], |r| {
        Ok(Count {
            label: r.get(0)?,
            value: r.get(1)?,
        })
    })?;
    collect_rows(rows)
}

fn collect_rows<T>(
    rows: rusqlite::MappedRows<'_, impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>>,
) -> Result<Vec<T>> {
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}
