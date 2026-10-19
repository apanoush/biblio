//! SQLite persistence.
//!
//! The database is a plain file that can be inspected with the `sqlite3` CLI.
//! Writes are done through a transaction by the caller for speed. The schema
//! is created idempotently on open.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};

use crate::merge::Extracted;
use crate::model::{FieldSource, Paper};

/// Per-connection / per-database pragmas applied on every open.
///
/// `journal_mode` persists in the database file, while `synchronous` and
/// `busy_timeout` are per-connection. WAL improves write throughput and lets
/// readers proceed while a scan writes.
const PRAGMAS: &str = r#"
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA busy_timeout = 5000;
PRAGMA foreign_keys = ON;
"#;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS papers (
    path        TEXT PRIMARY KEY,
    abs_path    TEXT NOT NULL,
    category    TEXT NOT NULL,
    subcategory TEXT,
    filetype    TEXT NOT NULL,
    title       TEXT,
    publisher   TEXT,
    doi         TEXT,
    arxiv_id    TEXT,
    arxiv_doi   TEXT,
    venue       TEXT,
    year        INTEGER,
    page_count  INTEGER,
    keywords    TEXT,
    size        INTEGER NOT NULL,
    mtime_ns    INTEGER NOT NULL,
    sha256      TEXT,
    first_seen  TEXT NOT NULL DEFAULT (datetime('now')),
    scanned_at  TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS authors (
    paper_path TEXT NOT NULL REFERENCES papers(path) ON DELETE CASCADE,
    position   INTEGER NOT NULL,
    name       TEXT NOT NULL,
    PRIMARY KEY (paper_path, position)
);

CREATE TABLE IF NOT EXISTS provenance (
    paper_path TEXT NOT NULL REFERENCES papers(path) ON DELETE CASCADE,
    field      TEXT NOT NULL,
    source     TEXT NOT NULL,
    confidence REAL NOT NULL,
    PRIMARY KEY (paper_path, field)
);

CREATE INDEX IF NOT EXISTS idx_papers_category ON papers(category);
CREATE INDEX IF NOT EXISTS idx_papers_year     ON papers(year);
CREATE INDEX IF NOT EXISTS idx_papers_doi      ON papers(doi);
CREATE INDEX IF NOT EXISTS idx_papers_arxiv    ON papers(arxiv_id);
CREATE INDEX IF NOT EXISTS idx_authors_name    ON authors(name);
"#;

/// A stored fingerprint used for incremental scanning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fingerprint {
    pub size: i64,
    pub mtime_ns: i64,
}

/// Open (creating if needed) the database at `path`.
pub fn open(path: &Path) -> Result<Connection> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating database directory {}", parent.display()))?;
    }
    let conn =
        Connection::open(path).with_context(|| format!("opening database {}", path.display()))?;
    init_schema(&conn)?;
    Ok(conn)
}

/// Apply pragmas, create the schema if needed, and run migrations.
pub fn init_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(PRAGMAS)
        .context("applying database pragmas")?;
    conn.execute_batch(SCHEMA)
        .context("creating database schema")?;
    migrate(conn)?;
    Ok(())
}

/// Bring an older database up to the current schema.
fn migrate(conn: &Connection) -> Result<()> {
    // `first_seen` was added after the initial release. `ALTER TABLE ... ADD
    // COLUMN` cannot use a non-constant default, so add it nullable and
    // backfill from the existing `scanned_at`.
    if !column_exists(conn, "papers", "first_seen")? {
        conn.execute_batch("ALTER TABLE papers ADD COLUMN first_seen TEXT")
            .context("adding papers.first_seen")?;
        conn.execute(
            "UPDATE papers SET first_seen = COALESCE(scanned_at, datetime('now'))
             WHERE first_seen IS NULL",
            [],
        )
        .context("backfilling papers.first_seen")?;
    }
    Ok(())
}

/// Whether `table` has a column named `column`.
fn column_exists(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let name: String = row.get(1)?;
        if name == column {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Path of every stored paper, in no particular order.
pub fn paths(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT path FROM papers")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Delete the given papers (authors and provenance cascade). Returns the number
/// of rows actually removed.
pub fn delete_paths(conn: &Connection, paths: &[String]) -> Result<usize> {
    if paths.is_empty() {
        return Ok(0);
    }
    let tx = conn.unchecked_transaction()?;
    let mut deleted = 0;
    {
        let mut stmt = tx.prepare("DELETE FROM papers WHERE path = ?1")?;
        for path in paths {
            deleted += stmt.execute([path])?;
        }
    }
    tx.commit().context("committing deletion transaction")?;
    Ok(deleted)
}

/// Load size/mtime fingerprints for every stored paper, keyed by path.
pub fn fingerprints(conn: &Connection) -> Result<HashMap<String, Fingerprint>> {
    let mut stmt = conn.prepare("SELECT path, size, mtime_ns FROM papers")?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            Fingerprint {
                size: row.get(1)?,
                mtime_ns: row.get(2)?,
            },
        ))
    })?;
    let mut map = HashMap::new();
    for row in rows {
        let (path, fp) = row?;
        map.insert(path, fp);
    }
    Ok(map)
}

/// Insert or update one paper, replacing its authors and provenance.
pub fn upsert(conn: &Connection, extracted: &Extracted) -> Result<()> {
    let paper = &extracted.paper;
    let keywords = serde_json::to_string(&paper.keywords)?;

    conn.execute(
        "INSERT INTO papers (
            path, abs_path, category, subcategory, filetype, title, publisher,
            doi, arxiv_id, arxiv_doi, venue, year, page_count, keywords,
            size, mtime_ns, sha256, scanned_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                   ?15, ?16, ?17, datetime('now'))
         ON CONFLICT(path) DO UPDATE SET
            abs_path=excluded.abs_path,
            category=excluded.category,
            subcategory=excluded.subcategory,
            filetype=excluded.filetype,
            title=excluded.title,
            publisher=excluded.publisher,
            doi=excluded.doi,
            arxiv_id=excluded.arxiv_id,
            arxiv_doi=excluded.arxiv_doi,
            venue=excluded.venue,
            year=excluded.year,
            page_count=excluded.page_count,
            keywords=excluded.keywords,
            size=excluded.size,
            mtime_ns=excluded.mtime_ns,
            sha256=excluded.sha256,
            scanned_at=datetime('now')",
        params![
            paper.path,
            paper.abs_path,
            paper.category,
            paper.subcategory,
            paper.filetype,
            paper.title,
            paper.publisher,
            paper.doi,
            paper.arxiv_id,
            paper.arxiv_doi,
            paper.venue,
            paper.year,
            paper.page_count,
            keywords,
            paper.size,
            paper.mtime_ns,
            paper.sha256,
        ],
    )
    .with_context(|| format!("upserting {}", paper.path))?;

    conn.execute("DELETE FROM authors WHERE paper_path = ?1", [&paper.path])?;
    for author in &paper.authors {
        conn.execute(
            "INSERT INTO authors (paper_path, position, name) VALUES (?1, ?2, ?3)",
            params![paper.path, author.position, author.name],
        )?;
    }

    conn.execute(
        "DELETE FROM provenance WHERE paper_path = ?1",
        [&paper.path],
    )?;
    for prov in &extracted.provenance {
        conn.execute(
            "INSERT INTO provenance (paper_path, field, source, confidence)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                paper.path,
                prov.field,
                prov.source.as_str(),
                prov.confidence
            ],
        )?;
    }

    Ok(())
}

/// Filters for [`query`]. All conditions are combined with AND.
#[derive(Debug, Default, Clone)]
pub struct Filter {
    /// Substring matched against title, DOI, arXiv id and author names.
    pub text: Option<String>,
    pub author: Option<String>,
    pub category: Option<String>,
    pub year: Option<i32>,
    /// Maximum number of rows; `None` means no limit.
    pub limit: Option<usize>,
    /// Number of rows to skip. Only meaningful together with an ordering.
    pub offset: Option<usize>,
}

/// A flattened row returned by [`query`].
#[derive(Debug, Clone, serde::Serialize)]
pub struct PaperRow {
    pub path: String,
    pub category: String,
    pub subcategory: Option<String>,
    pub filetype: String,
    pub title: Option<String>,
    pub authors: String,
    pub publisher: Option<String>,
    pub doi: Option<String>,
    pub arxiv_id: Option<String>,
    pub arxiv_doi: Option<String>,
    pub venue: Option<String>,
    pub year: Option<i32>,
    pub page_count: Option<i64>,
    pub keywords: Option<String>,
}

/// Run a filtered query, returning flattened rows ordered by path.
pub fn query(conn: &Connection, filter: &Filter) -> Result<Vec<PaperRow>> {
    let mut sql = String::from(
        "SELECT p.path, p.category, p.subcategory, p.filetype, p.title,
                COALESCE(GROUP_CONCAT(a.name, '; '), '') AS authors,
                p.publisher, p.doi, p.arxiv_id, p.arxiv_doi, p.venue, p.year,
                p.page_count, p.keywords
         FROM papers p
         LEFT JOIN authors a ON a.paper_path = p.path
         WHERE 1 = 1",
    );
    let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    if let Some(category) = &filter.category {
        sql.push_str(" AND p.category = ?");
        args.push(Box::new(category.clone()));
    }
    if let Some(year) = filter.year {
        sql.push_str(" AND p.year = ?");
        args.push(Box::new(year));
    }
    if let Some(author) = &filter.author {
        sql.push_str(
            " AND EXISTS (SELECT 1 FROM authors a2
                          WHERE a2.paper_path = p.path AND a2.name LIKE ?)",
        );
        args.push(Box::new(format!("%{author}%")));
    }
    if let Some(text) = &filter.text {
        let like = format!("%{text}%");
        // The same pattern is bound to each of the four placeholders.
        sql.push_str(
            " AND (p.title LIKE ? OR p.doi LIKE ? OR p.arxiv_id LIKE ?
                   OR EXISTS (SELECT 1 FROM authors a3
                              WHERE a3.paper_path = p.path AND a3.name LIKE ?))",
        );
        for _ in 0..3 {
            args.push(Box::new(like.clone()));
        }
        args.push(Box::new(like));
    }

    sql.push_str(" GROUP BY p.path ORDER BY p.path");
    match (filter.limit, filter.offset) {
        (Some(limit), Some(offset)) => sql.push_str(&format!(" LIMIT {limit} OFFSET {offset}")),
        (Some(limit), None) => sql.push_str(&format!(" LIMIT {limit}")),
        (None, Some(offset)) => sql.push_str(&format!(" LIMIT -1 OFFSET {offset}")),
        (None, None) => {}
    }

    let mut stmt = conn.prepare(&sql)?;
    let param_refs: Vec<&dyn rusqlite::ToSql> = args.iter().map(|b| b.as_ref()).collect();
    let rows = stmt.query_map(param_refs.as_slice(), |row| {
        Ok(PaperRow {
            path: row.get(0)?,
            category: row.get(1)?,
            subcategory: row.get(2)?,
            filetype: row.get(3)?,
            title: row.get(4)?,
            authors: row.get(5)?,
            publisher: row.get(6)?,
            doi: row.get(7)?,
            arxiv_id: row.get(8)?,
            arxiv_doi: row.get(9)?,
            venue: row.get(10)?,
            year: row.get(11)?,
            page_count: row.get(12)?,
            keywords: row.get(13)?,
        })
    })?;

    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Count rows in the `papers` table.
pub fn count(conn: &Connection) -> Result<i64> {
    let n: Option<i64> = conn
        .query_row("SELECT COUNT(*) FROM papers", [], |r| r.get(0))
        .optional()?;
    Ok(n.unwrap_or(0))
}

/// Map of field name -> number of non-null values, for coverage reporting.
pub fn coverage(conn: &Connection) -> Result<Vec<(String, i64)>> {
    let mut out = Vec::new();
    for field in ["title", "doi", "arxiv_id", "publisher", "venue", "year"] {
        let sql = format!("SELECT COUNT({field}) FROM papers");
        let n: i64 = conn.query_row(&sql, [], |r| r.get(0))?;
        out.push((field.to_string(), n));
    }
    let authors: i64 =
        conn.query_row("SELECT COUNT(DISTINCT paper_path) FROM authors", [], |r| {
            r.get(0)
        })?;
    out.push(("authors".to_string(), authors));
    Ok(out)
}

/// Map of category -> count.
pub fn category_counts(conn: &Connection) -> Result<Vec<(String, i64)>> {
    let mut stmt = conn.prepare(
        "SELECT category, COUNT(*) FROM papers GROUP BY category ORDER BY COUNT(*) DESC",
    )?;
    let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Used by tests and reporting to read back a raw paper.
pub fn get_paper(conn: &Connection, path: &str) -> Result<Option<Paper>> {
    let mut stmt = conn.prepare(
        "SELECT path, abs_path, category, subcategory, filetype, title, publisher,
                doi, arxiv_id, arxiv_doi, venue, year, page_count, keywords,
                size, mtime_ns, sha256
         FROM papers WHERE path = ?1",
    )?;
    let mut rows = stmt.query_map([path], |row| {
        let keywords_json: Option<String> = row.get(13)?;
        Ok(Paper {
            path: row.get(0)?,
            abs_path: row.get(1)?,
            category: row.get(2)?,
            subcategory: row.get(3)?,
            filetype: row.get(4)?,
            title: row.get(5)?,
            authors: Vec::new(),
            publisher: row.get(6)?,
            doi: row.get(7)?,
            arxiv_id: row.get(8)?,
            arxiv_doi: row.get(9)?,
            venue: row.get(10)?,
            year: row.get(11)?,
            page_count: row.get(12)?,
            keywords: keywords_json
                .and_then(|j| serde_json::from_str(&j).ok())
                .unwrap_or_default(),
            size: row.get(14)?,
            mtime_ns: row.get(15)?,
            sha256: row.get(16)?,
        })
    })?;
    match rows.next() {
        Some(p) => Ok(Some(p?)),
        None => Ok(None),
    }
}

/// Provenance of a single field, exposed for audits.
pub fn provenance_for(conn: &Connection, path: &str, field: &str) -> Result<Option<FieldSource>> {
    let mut stmt =
        conn.prepare("SELECT source FROM provenance WHERE paper_path = ?1 AND field = ?2")?;
    let src: Option<String> = stmt
        .query_row(params![path, field], |r| r.get(0))
        .optional()?;
    Ok(src.map(|s| match s.as_str() {
        "embedded" => FieldSource::Embedded,
        "filename" => FieldSource::Filename,
        "path" => FieldSource::Path,
        _ => FieldSource::Derived,
    }))
}
