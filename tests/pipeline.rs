//! End-to-end test of the discovery -> filename -> merge -> database pipeline.
//!
//! Embedded metadata extraction (exiftool) is intentionally bypassed here: the
//! test files are not real PDFs, so each paper is merged from its path and file
//! name only.

use std::path::{Path, PathBuf};

use biblio::config::Config;
use biblio::metadata::Metadata;
use biblio::sync::{self, NoProgress, SyncOptions};
use biblio::{classify, db, discover, filename, merge, stats};

/// Create a unique temporary workspace and return its path.
fn temp_workspace(tag: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!("biblio_it_{}_{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    path
}

#[test]
fn scan_pipeline_roundtrip() {
    let workspace = temp_workspace("roundtrip");
    let papers = workspace.join("papers");
    std::fs::create_dir_all(papers.join("preprints/pdfs")).unwrap();
    std::fs::create_dir_all(papers.join("journal/pdfs")).unwrap();

    std::fs::write(
        papers.join("preprints/pdfs/2506.07751v4.pdf"),
        b"%PDF-1.4 fake",
    )
    .unwrap();
    std::fs::write(
        papers.join("journal/pdfs/less_is_more.pdf"),
        b"%PDF-1.4 fake",
    )
    .unwrap();

    let files = discover::discover(&papers, &["pdf".to_string()], false).unwrap();
    assert_eq!(files.len(), 2);

    let conn = db::open(&workspace.join("data/biblio.db")).unwrap();
    let tx = conn.unchecked_transaction().unwrap();
    for file in &files {
        let stem = Path::new(&file.rel_path)
            .file_stem()
            .unwrap()
            .to_str()
            .unwrap();
        let parsed = filename::parse(stem);
        let extracted = merge::build(file, None, &parsed);
        db::upsert(&tx, &extracted).unwrap();
    }
    tx.commit().unwrap();

    assert_eq!(db::count(&conn).unwrap(), 2);
    assert_eq!(db::fingerprints(&conn).unwrap().len(), 2);

    let rows = db::query(
        &conn,
        &db::Filter {
            category: Some("preprints".to_string()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].arxiv_id.as_deref(), Some("2506.07751"));
    assert_eq!(
        rows[0].arxiv_doi.as_deref(),
        Some("10.48550/arXiv.2506.07751")
    );

    std::fs::remove_dir_all(&workspace).ok();
}

/// Insert every discovered file into `conn`, using file-name metadata only.
fn store_all(conn: &rusqlite::Connection, files: &[discover::DiscoveredFile]) {
    let tx = conn.unchecked_transaction().unwrap();
    for file in files {
        let stem = Path::new(&file.rel_path)
            .file_stem()
            .unwrap()
            .to_str()
            .unwrap();
        let parsed = filename::parse(stem);
        let extracted = merge::build(file, None, &parsed);
        db::upsert(&tx, &extracted).unwrap();
    }
    tx.commit().unwrap();
}

#[test]
fn wal_mode_is_enabled() {
    let workspace = temp_workspace("wal");
    let conn = db::open(&workspace.join("data/biblio.db")).unwrap();
    let mode: String = conn
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode.to_ascii_lowercase(), "wal");
    std::fs::remove_dir_all(&workspace).ok();
}

#[test]
fn prune_removes_rows_whose_file_is_gone() {
    let workspace = temp_workspace("prune");
    let papers = workspace.join("papers");
    std::fs::create_dir_all(papers.join("journal/pdfs")).unwrap();
    std::fs::write(papers.join("journal/pdfs/keep_me.pdf"), b"%PDF-1.4 fake").unwrap();
    std::fs::write(papers.join("journal/pdfs/delete_me.pdf"), b"%PDF-1.4 fake").unwrap();

    let conn = db::open(&workspace.join("data/biblio.db")).unwrap();
    let files = discover::discover(&papers, &["pdf".to_string()], false).unwrap();
    store_all(&conn, &files);
    assert_eq!(db::count(&conn).unwrap(), 2);

    // One file disappears, then a scan discovers only the survivor.
    std::fs::remove_file(papers.join("journal/pdfs/delete_me.pdf")).unwrap();
    let remaining = discover::discover(&papers, &["pdf".to_string()], false).unwrap();
    assert_eq!(remaining.len(), 1);

    let present: std::collections::HashSet<&str> =
        remaining.iter().map(|f| f.rel_path.as_str()).collect();
    let stale: Vec<String> = db::paths(&conn)
        .unwrap()
        .into_iter()
        .filter(|p| !present.contains(p.as_str()))
        .collect();
    let deleted = db::delete_paths(&conn, &stale).unwrap();

    assert_eq!(deleted, 1);
    assert_eq!(db::count(&conn).unwrap(), 1);
    let rows = db::query(&conn, &db::Filter::default()).unwrap();
    assert_eq!(rows.len(), 1);
    assert!(rows[0].path.ends_with("keep_me.pdf"));

    std::fs::remove_dir_all(&workspace).ok();
}

#[test]
fn sync_orchestration_is_incremental_and_prunes() {
    let workspace = temp_workspace("sync");
    let papers = workspace.join("papers");
    std::fs::create_dir_all(papers.join("preprints/pdfs")).unwrap();
    std::fs::create_dir_all(papers.join("journal/pdfs")).unwrap();
    std::fs::write(
        papers.join("preprints/pdfs/2506.07751v4.pdf"),
        b"%PDF-1.4 fake",
    )
    .unwrap();
    std::fs::write(
        papers.join("journal/pdfs/less_is_more.pdf"),
        b"%PDF-1.4 fake",
    )
    .unwrap();

    let conn = db::open(&workspace.join("data/biblio.db")).unwrap();
    let cfg = Config {
        papers_root: papers.clone(),
        batch_size: 1,
        ..Config::default()
    };
    let opts = SyncOptions::default();

    let first = sync::sync(&conn, &cfg, &opts, &NoProgress).unwrap();
    assert_eq!(first.discovered, 2);
    assert_eq!(first.inserted, 2);
    assert_eq!(first.updated, 0);
    assert_eq!(first.unchanged, 0);
    assert_eq!(db::count(&conn).unwrap(), 2);

    // Second run sees everything unchanged.
    let second = sync::sync(&conn, &cfg, &opts, &NoProgress).unwrap();
    assert_eq!(second.unchanged, 2);
    assert_eq!(second.inserted, 0);

    // Remove a file and prune.
    std::fs::remove_file(papers.join("journal/pdfs/less_is_more.pdf")).unwrap();
    let prune_opts = SyncOptions {
        prune: true,
        ..SyncOptions::default()
    };
    let third = sync::sync(&conn, &cfg, &prune_opts, &NoProgress).unwrap();
    assert_eq!(third.discovered, 1);
    assert_eq!(third.deleted, 1);
    assert_eq!(db::count(&conn).unwrap(), 1);

    std::fs::remove_dir_all(&workspace).ok();
}

#[test]
fn first_seen_is_preserved_on_update() {
    let workspace = temp_workspace("first_seen");
    let papers = workspace.join("papers");
    std::fs::create_dir_all(papers.join("journal/pdfs")).unwrap();
    std::fs::write(papers.join("journal/pdfs/keep_me.pdf"), b"%PDF-1.4 fake").unwrap();

    let conn = db::open(&workspace.join("data/biblio.db")).unwrap();
    let files = discover::discover(&papers, &["pdf".to_string()], false).unwrap();
    store_all(&conn, &files);

    conn.execute("UPDATE papers SET first_seen = 'SENTINEL'", [])
        .unwrap();
    // Re-upsert the same row; first_seen must survive.
    store_all(&conn, &files);
    let first_seen: String = conn
        .query_row("SELECT first_seen FROM papers LIMIT 1", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(first_seen, "SENTINEL");

    std::fs::remove_dir_all(&workspace).ok();
}

#[test]
fn query_offset_pages_rows() {
    let workspace = temp_workspace("paging");
    let papers = workspace.join("papers");
    std::fs::create_dir_all(papers.join("journal/pdfs")).unwrap();
    for name in ["a.pdf", "b.pdf", "c.pdf"] {
        std::fs::write(papers.join("journal/pdfs").join(name), b"%PDF-1.4 fake").unwrap();
    }

    let conn = db::open(&workspace.join("data/biblio.db")).unwrap();
    let files = discover::discover(&papers, &["pdf".to_string()], false).unwrap();
    store_all(&conn, &files);

    let page0 = db::query(
        &conn,
        &db::Filter {
            limit: Some(1),
            offset: Some(0),
            ..Default::default()
        },
    )
    .unwrap();
    let page1 = db::query(
        &conn,
        &db::Filter {
            limit: Some(1),
            offset: Some(1),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(page0.len(), 1);
    assert_eq!(page1.len(), 1);
    assert_ne!(page0[0].path, page1[0].path);

    // Offset without a limit returns the remaining rows.
    let rest = db::query(
        &conn,
        &db::Filter {
            offset: Some(1),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(rest.len(), 2);

    std::fs::remove_dir_all(&workspace).ok();
}

/// Insert a synthetic paper with the given embedded title and DOI.
fn insert_paper(conn: &rusqlite::Connection, rel_path: &str, title: &str, doi: &str) {
    let classification = classify::classify(Path::new(rel_path));
    let file = discover::DiscoveredFile {
        rel_path: rel_path.to_string(),
        abs_path: Path::new("/papers").join(rel_path),
        size: 10,
        mtime_ns: 0,
        classification,
        sha256: None,
    };
    let meta = Metadata {
        title: Some(title.to_string()),
        doi: Some(doi.to_string()),
        ..Metadata::default()
    };
    let stem = Path::new(rel_path).file_stem().unwrap().to_str().unwrap();
    let parsed = filename::parse(stem);
    let extracted = merge::build(&file, Some(&meta), &parsed);
    db::upsert(conn, &extracted).unwrap();
}

#[test]
fn stats_collect_reports_aggregates_and_anomalies() {
    let workspace = temp_workspace("stats");
    let conn = db::open(&workspace.join("data/biblio.db")).unwrap();

    insert_paper(&conn, "journal/pdfs/foo2020.pdf", "Paper One", "10.1/x");
    insert_paper(&conn, "journal/pdfs/bar2024.pdf", "Paper Two", "10.1/x");
    insert_paper(&conn, "preprints/pdfs/baz2097.pdf", "Paper Three", "10.2/y");

    let s = stats::collect(&conn, 10).unwrap();
    assert_eq!(s.total_papers, 3);
    assert_eq!(
        s.by_category
            .iter()
            .find(|c| c.label == "journal")
            .unwrap()
            .value,
        2
    );
    assert_eq!(s.distinct_dois, 2);

    // The implausible year is excluded from the timeline and listed as an anomaly.
    assert!(s.by_year.iter().all(|c| c.label != "2097"));
    assert_eq!(s.anomalies.implausible_years.len(), 1);
    assert_eq!(s.anomalies.implausible_years[0].year, 2097);

    // Duplicate DOI detected.
    assert_eq!(s.anomalies.duplicate_dois.len(), 1);
    assert_eq!(s.anomalies.duplicate_dois[0].label, "10.1/x");

    // Coverage counts the embedded titles.
    let title = s.coverage.iter().find(|c| c.field == "title").unwrap();
    assert_eq!(title.present, 3);
    assert_eq!(title.total, 3);

    std::fs::remove_dir_all(&workspace).ok();
}
