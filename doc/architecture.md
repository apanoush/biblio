# Architecture

## Overview

`biblio` is a small single-crate pipeline. Each stage is a pure-ish function
over plain data structures, which keeps stages independently testable.

```
discover -> classify -> +-- metadata (exiftool) --+
                        |                          |
                        +-- filename (regexes) ----+-> merge -> db (SQLite)
```

## Configuration

Configuration is resolved by `src/config.rs` in layers (defaults, user-global
file, project file found by walking up from the working directory, explicit
`--config`, environment, flags). Each file is parsed into an all-optional
overlay and merged field by field. The path fields are expanded for `~`,
`$VAR` and `${VAR}` (via `shellexpand`) before the environment overrides are
applied. The user-global directory comes from `directories::ProjectDirs`.
`biblio config` prints the effective configuration and the files that
produced it.

## Stages

1. **discover** (`src/discover.rs`) walks the papers root with `walkdir`
   (not following symlinks), keeps files whose lowercased extension is in
   `include_extensions`, and records relative path, absolute path, size and
   modification time. Output is sorted by relative path for determinism.
2. **classify** (`src/classify.rs`) derives `category` from the top-level
   folder, an optional `subcategory` from a second-level folder (ignoring the
   generic `pdfs/` container), and `filetype` from the extension.
3. **metadata** (`src/metadata.rs`) runs `exiftool -j` over batches of files
   and maps the returned JSON to a `Metadata` struct. Both the PDF Info
   dictionary and XMP are covered because exiftool normalises them.
4. **filename** (`src/filename.rs`) applies ordered regex strategies to the
   file stem: arXiv id, ACL/venue id, NeurIPS slug, ACM id, Springer id,
   DOI-encoded, author-year, then a readable-title fallback.
5. **merge** (`src/merge.rs`) combines embedded metadata (higher priority) and
   file-name data (lower priority) with path-derived fields (authoritative),
   and derives `arxiv_id` / `arxiv_doi`. It records a `Provenance` entry per
   assigned field.
6. **db** (`src/db.rs`) upserts into SQLite inside a caller-managed
   transaction, replacing the paper's author and provenance rows.
7. **sync** (`src/sync.rs`) orchestrates the whole run (discover, parallel
   extract, serial store, optional prune), reports progress through the
   `Progress` trait and returns a `ScanStats` summary. Keeping terminal
   concerns behind the trait keeps the library free of UI dependencies.

## Data model

`Paper` (see `src/model.rs`) is the central structure: identity (`path`,
`abs_path`), classification (`category`, `subcategory`, `filetype`),
bibliographic fields, and file facts (`size`, `mtime_ns`, `sha256`).

Each `papers` row also carries `first_seen` (set on insert and preserved on
update) and `scanned_at` (last refresh).

The database normalises authors and provenance into their own tables so they
can be indexed and queried (for example, filter or search by author).

## Priority and provenance

For a field available from several sources the priority is:

```
embedded metadata  >  file name  >  (derived)
```

Path-derived classification is always taken from the tree. Confidence values
are centralised in `src/merge.rs` (`CONF_EMBEDDED`, `CONF_FILENAME`).

## Incremental scanning and pruning

`papers.path` is the stable key. Before extracting, the scan loads
`(path, size, mtime_ns)` fingerprints and skips unchanged files unless
`--force` is passed. This makes repeated scans cheap while remaining correct
when files change.

By default the scan never deletes. With `--prune`, rows whose file is no longer
present on disk are removed (`db::delete_paths`, cascading to authors and
provenance); the count is reported as `deleted`.

## Concurrency

Extraction (the slow phase, dominated by `exiftool`) runs in parallel with
`rayon`: `to_process` is split into batches and each batch is processed by one
`exiftool` invocation. Results are collected in order. Database writes are
serialized on a single transaction to avoid lock contention.

Progress is reported through the `Progress` trait (`src/sync.rs`): a spinner
while a phase total is unknown, a bar once it is known, cleared at phase end.
The CLI supplies an indicatif-backed implementation and hides it when stderr is
not a terminal.

## Storage

The database runs in SQLite WAL mode with `synchronous=NORMAL` and a 5s busy
timeout (set on every open in `db.rs`). WAL improves write throughput and keeps
the database readable during a scan; it creates `-wal` and `-shm` sidecar files
while open.

## Statistics and UI

`src/stats.rs` computes aggregate statistics (`Stats`) from pure SQL. The CLI
renders it as text or JSON (`stats`), and the interactive dashboard
(`biblio tui`, implemented in the binary's `src/tui.rs`) renders the same
structure with ratatui. Rendering is a pure function of the app state so it can
be tested with ratatui's `TestBackend`.

## Read-only guarantee

The papers root is only ever read: `discover` opens files read-only and only
when `hash_contents` is enabled. `exiftool` is invoked in read-only mode (no
write flags), so it never creates `_original` backups.
