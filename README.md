# biblio

`biblio` scans a filesystem tree of academic papers, extracts bibliographic
metadata from embedded document metadata and from the file path/name, and
aggregates everything into a local SQLite database.

The paper tree is treated as **read-only**; the database lives in this project.

## What is extracted

- title, authors, publisher
- DOI, arXiv identifier and arXiv DOI
- publication category (`journal`, `conference`, `preprints`, `workshops`, ...)
  derived from the containing folder
- file type (`pdf`, `html`, ...)
- venue and year, when recoverable from the file name
- page count and keywords, when present in the document

Each field records its source (embedded metadata, file name, path, or derived)
and a confidence score in the `provenance` table.

## Requirements

- Rust (edition 2024, tested with 1.93)
- `exiftool` on `PATH` (used to read embedded PDF/XMP metadata)

## Build

```sh
cargo build --release
```

## Configuration

Configuration is layered. Later layers override earlier ones:

1. built-in defaults
2. user-global file: `$XDG_CONFIG_HOME/biblio/config.toml`, else
   `~/.config/biblio/config.toml`
3. a project file, discovered by walking up from the current directory to the
   nearest ancestor containing `config.toml`
4. an explicit `--config PATH` (this disables project auto-discovery)
5. environment variables: `papers` / `PAPERS` (papers root), `BIBLIO_DB`
6. command-line flags

Each file only needs to set the keys it wants to change. In `papers_root` and
`db_path`, a leading `~` and `$VAR` / `${VAR}` references are expanded from the
environment; referencing an unset variable is an error.

Run `biblio config` to print the resolved configuration, the files that
contributed to it, and any overrides applied.

| key | default | meaning |
|---|---|---|
| `papers_root` | `$papers` | root of the paper tree (read-only) |
| `db_path` | `data/biblio.db` | SQLite file |
| `include_extensions` | `["pdf","html","htm"]` | files to consider |
| `batch_size` | `100` | files per `exiftool` call |
| `hash_contents` | `false` | also compute SHA-256 per file |

These keys are the same in the user-global and project files; see the table
above. See `config.toml.example` for an annotated template.

## Usage

```sh
# Sync the database with the papers tree.
biblio sync

# Re-extract everything, ignoring stored fingerprints.
biblio sync --force

# Remove database rows whose file no longer exists on disk.
biblio sync --prune

# Disable the progress display (hidden automatically when not a TTY).
biblio sync --no-progress

# Show per-category counts and field coverage.
biblio sync --verbose

# List the catalog, page by page.
biblio list --limit 20 --offset 0
biblio list --category preprints

# Search.
biblio query --text "attention" --limit 5
biblio query --author "Bengio"
biblio query --category preprints --year 2025
biblio query --json

# Export.
biblio export --format csv  --out papers.csv
biblio export --format json --out papers.json

# Statistics (text or JSON) and an interactive dashboard.
biblio stats
biblio stats --json
biblio tui

# Show the resolved configuration and where it came from.
biblio config

# Write a template config file (refuses to overwrite without --force).
biblio config --init            # ./config.toml
biblio config --init --global   # user-global (~/.config/biblio/config.toml)
```

All subcommands accept `--config PATH` and `--db PATH`; `sync` (also available
as `scan`) additionally accepts `--root PATH`, `--force`, `--hash`, `--prune`,
`--no-progress` and `--verbose`.

Sync is incremental: a file is re-extracted only when its size or modification
time changed. Extraction runs in parallel (one `exiftool` call per batch), while
database writes are serialized. At the end it prints one summary line, for
example:

```
Imported 658 papers (658 new, 0 updated), 0 unchanged, removed 0 stale
```

`sync --verbose` adds the per-category breakdown and field coverage. The first
time a paper is seen its `first_seen` timestamp is recorded and preserved on
later updates.

## Statistics

`biblio stats` prints catalog statistics: totals, per-category and per-year
distributions, field coverage, top authors and venues, a per-field provenance
breakdown (embedded vs file name vs derived), growth over time, and data-health
anomalies (implausible years, missing title+DOI, duplicate DOIs). `--json`
emits the same as JSON.

`biblio tui` opens the same statistics as an interactive dashboard:

| key | action |
|---|---|
| `q` / `Esc` | quit |
| `Tab`, `←` `→` | next / previous tab |
| `1`-`5` | jump to a tab (Overview, Timeline, People, Venues, Health) |
| `j` / `k` | scroll down / up |
| `?` | toggle help |

The database uses SQLite **WAL** mode (`synchronous=NORMAL`, 5s busy timeout),
which is faster and lets reads proceed during a scan. WAL creates `-wal` and
`-shm` sidecar files next to the database while it is open; both are ignored by
git.

## Database schema

- `papers` - one row per file, keyed by the path relative to the papers root.
- `authors` - `(paper_path, position, name)`.
- `provenance` - `(paper_path, field, source, confidence)`.

The database is a normal SQLite file and can be queried with `sqlite3`.

## Development

```sh
cargo fmt
cargo clippy --all-targets
cargo test
```

See `doc/index.md` for the design overview.
