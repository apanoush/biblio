# biblio documentation

`biblio` aggregates bibliographic metadata from a filesystem tree of papers
into a local SQLite database.

## Documents

- [architecture.md](architecture.md) - pipeline, modules, data model.
- [decisions/](decisions/README.md) - concise decision records (context, rationale, alternatives, consequences).
- [../README.md](../README.md) - usage and configuration.

## Modules

| Module | Responsibility |
|---|---|
| `config` | load `config.toml` and environment overrides |
| `discover` | walk the tree, collect files with size/mtime |
| `classify` | map a relative path to category / file type |
| `metadata` | read embedded metadata via `exiftool` |
| `filename` | recover metadata from file names |
| `merge` | combine sources into a `Paper` with provenance |
| `db` | SQLite schema, upsert and query |
| `sync` | scan orchestration (discover, extract, store, prune) |
| `stats` | aggregate statistics over the catalog |
| `report` | scan statistics |
| `model` | shared data structures |

## Scope

In scope: offline extraction from embedded metadata and file names, one SQLite
database, a `sync`/`list`/`query`/`export`/`stats`/`tui`/`config` CLI.

Out of scope (possible future work): network enrichment from CrossRef/arXiv,
full-text extraction, HTML sources, a GUI.
