//! Command-line entry point for `biblio`.
//!
//! This module owns argument parsing, terminal output and progress rendering.
//! Domain logic lives in the `biblio` library, notably `biblio::sync`.

use std::io::IsTerminal;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use console::style;
use indicatif::{ProgressBar, ProgressStyle};

use biblio::config::Config;
use biblio::db::{self, Filter};
use biblio::report::{self, ScanStats};
use biblio::stats::{self, Stats};
use biblio::sync::{self, Progress, SyncOptions};

mod tui;

/// Gather paper metadata from a filesystem tree into a SQLite database.
#[derive(Parser)]
#[command(name = "biblio", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Sync the database with the papers tree (discover, extract, store).
    #[command(alias = "scan")]
    Sync(SyncArgs),
    /// List the catalog, page by page.
    List(ListArgs),
    /// Search the catalog by text or field.
    Query(QueryArgs),
    /// Export the database to CSV or JSON.
    Export(ExportArgs),
    /// Print catalog statistics.
    Stats(StatsArgs),
    /// Interactive statistics dashboard.
    Tui(TuiArgs),
    /// Print the resolved configuration and its sources.
    Config(ConfigArgs),
}

#[derive(Args)]
struct CommonArgs {
    /// Path to an explicit config file (disables project auto-discovery).
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    /// Override the database path.
    #[arg(long, global = true)]
    db: Option<PathBuf>,
}

#[derive(Args)]
struct SyncArgs {
    #[command(flatten)]
    common: CommonArgs,
    /// Override the papers root.
    #[arg(long)]
    root: Option<PathBuf>,
    /// Re-extract every file, ignoring stored fingerprints.
    #[arg(long)]
    force: bool,
    /// Compute a SHA-256 content hash for each file.
    #[arg(long)]
    hash: bool,
    /// Remove stored rows whose file no longer exists on disk.
    #[arg(long)]
    prune: bool,
    /// Disable the progress display.
    #[arg(long)]
    no_progress: bool,
    /// Print per-category counts and field coverage.
    #[arg(long)]
    verbose: bool,
}

#[derive(Args)]
struct ListArgs {
    #[command(flatten)]
    common: CommonArgs,
    /// Filter by category folder.
    #[arg(long)]
    category: Option<String>,
    /// Filter by publication year.
    #[arg(long)]
    year: Option<i32>,
    /// Maximum number of rows (default 50).
    #[arg(long, default_value_t = 50)]
    limit: usize,
    /// Number of rows to skip (default 0).
    #[arg(long, default_value_t = 0)]
    offset: usize,
    /// Emit JSON instead of a human-readable list.
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
struct QueryArgs {
    #[command(flatten)]
    common: CommonArgs,
    /// Free-text search over title, DOI, arXiv id and authors.
    #[arg(long)]
    text: Option<String>,
    /// Filter by author name (substring).
    #[arg(long)]
    author: Option<String>,
    /// Filter by category folder.
    #[arg(long)]
    category: Option<String>,
    /// Filter by publication year.
    #[arg(long)]
    year: Option<i32>,
    /// Maximum number of results (default 50).
    #[arg(long, default_value_t = 50)]
    limit: usize,
    /// Number of results to skip (default 0).
    #[arg(long, default_value_t = 0)]
    offset: usize,
    /// Emit JSON instead of a human-readable list.
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
struct ExportArgs {
    #[command(flatten)]
    common: CommonArgs,
    /// Output format.
    #[arg(long, value_enum, default_value_t = ExportFormat::Csv)]
    format: ExportFormat,
    /// Output file (defaults to stdout).
    #[arg(long)]
    out: Option<PathBuf>,
}

#[derive(Args)]
struct ConfigArgs {
    #[command(flatten)]
    common: CommonArgs,
    /// Emit JSON instead of TOML.
    #[arg(long)]
    json: bool,
    /// Write a template config file and exit.
    #[arg(long)]
    init: bool,
    /// With --init, target the user-global file instead of ./config.toml.
    #[arg(long, requires = "init")]
    global: bool,
    /// With --init, overwrite an existing file.
    #[arg(long, requires = "init")]
    force: bool,
}

#[derive(Args)]
struct StatsArgs {
    #[command(flatten)]
    common: CommonArgs,
    /// Emit JSON instead of text.
    #[arg(long)]
    json: bool,
    /// Number of rows in ranked lists (authors, venues).
    #[arg(long, default_value_t = 10)]
    top: i64,
}

#[derive(Args)]
struct TuiArgs {
    #[command(flatten)]
    common: CommonArgs,
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum ExportFormat {
    Csv,
    Json,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    let cli = Cli::parse();
    match cli.command {
        Commands::Sync(args) => run_sync(args),
        Commands::List(args) => run_list(args),
        Commands::Query(args) => run_query(args),
        Commands::Export(args) => run_export(args),
        Commands::Stats(args) => run_stats(args),
        Commands::Tui(args) => run_tui(args),
        Commands::Config(args) => run_config(args),
    }
}

/// Load config and apply command-line overrides common to all subcommands.
fn resolve_config(common: &CommonArgs) -> Result<Config> {
    let mut cfg = Config::load(common.config.as_deref())?;
    if let Some(db) = &common.db {
        cfg.db_path = db.clone();
    }
    Ok(cfg)
}

// --- sync ---------------------------------------------------------------

fn run_sync(args: SyncArgs) -> Result<()> {
    let mut cfg = resolve_config(&args.common)?;
    if let Some(root) = &args.root {
        cfg.papers_root = root.clone();
    }
    if args.hash {
        cfg.hash_contents = true;
    }
    cfg.validate()?;

    let opts = SyncOptions {
        force: args.force,
        prune: args.prune,
        hash_contents: cfg.hash_contents,
        batch_size: cfg.batch_size,
    };

    let conn = db::open(&cfg.db_path)?;
    let progress = IndicatifProgress::new(!args.no_progress && std::io::stderr().is_terminal());
    let stats = sync::sync(&conn, &cfg, &opts, &progress)?;

    print_summary(&stats);
    if args.verbose {
        report::print_details(&stats, &conn)?;
    }
    Ok(())
}

/// One-line import summary with bolded counts.
fn print_summary(stats: &ScanStats) {
    let mut line = format!(
        "Imported {} papers ({} new, {} updated), {} unchanged, removed {} stale",
        style(stats.imported()).bold(),
        style(stats.inserted).bold(),
        style(stats.updated).bold(),
        style(stats.unchanged).bold(),
        style(stats.deleted).bold(),
    );
    if stats.errors > 0 {
        line.push_str(&format!(", {} errors", style(stats.errors).bold()));
    }
    println!("{line}");
}

/// Terminal progress: a spinner for unknown totals, a bar for known totals.
struct IndicatifProgress {
    spinner: ProgressBar,
    bar: ProgressBar,
}

impl IndicatifProgress {
    fn new(enabled: bool) -> Self {
        let (spinner, bar) = if enabled {
            (ProgressBar::new_spinner(), ProgressBar::new(0))
        } else {
            (ProgressBar::hidden(), ProgressBar::hidden())
        };
        Self { spinner, bar }
    }
}

impl Progress for IndicatifProgress {
    fn start_spinner(&self, message: &str) {
        self.spinner
            .set_style(ProgressStyle::with_template("{spinner:.green} {msg}").expect("template"));
        self.spinner.set_message(message.to_string());
        self.spinner.enable_steady_tick(Duration::from_millis(100));
    }

    fn start_bar(&self, message: &str, total: u64) {
        self.bar.set_style(
            ProgressStyle::with_template("{spinner:.green} [{bar:40.cyan/blue}] {pos}/{len} {msg}")
                .expect("template")
                .progress_chars("=>-"),
        );
        self.bar.set_length(total);
        self.bar.set_position(0);
        self.bar.set_message(message.to_string());
        self.bar.enable_steady_tick(Duration::from_millis(100));
    }

    fn inc(&self, n: u64) {
        self.bar.inc(n);
    }

    fn finish(&self) {
        self.spinner.finish_and_clear();
        self.bar.finish_and_clear();
    }
}

// --- list / query -------------------------------------------------------

fn run_list(args: ListArgs) -> Result<()> {
    let cfg = resolve_config(&args.common)?;
    let conn = db::open(&cfg.db_path)?;
    let filter = Filter {
        category: args.category,
        year: args.year,
        limit: Some(args.limit),
        offset: Some(args.offset),
        ..Filter::default()
    };
    let rows = db::query(&conn, &filter)?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }

    println!("{} row(s)", rows.len());
    for row in &rows {
        let title = row.title.as_deref().unwrap_or("(no title)");
        println!("{}\t{}", row.path, title);
    }
    Ok(())
}

fn run_query(args: QueryArgs) -> Result<()> {
    let cfg = resolve_config(&args.common)?;
    let conn = db::open(&cfg.db_path)?;
    let filter = Filter {
        text: args.text,
        author: args.author,
        category: args.category,
        year: args.year,
        limit: Some(args.limit),
        offset: Some(args.offset),
    };
    let rows = db::query(&conn, &filter)?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }

    println!("{} result(s)", rows.len());
    for row in &rows {
        let title = row.title.as_deref().unwrap_or("(no title)");
        let mut meta_bits = Vec::new();
        if let Some(year) = row.year {
            meta_bits.push(year.to_string());
        }
        if let Some(venue) = &row.venue {
            meta_bits.push(venue.clone());
        }
        let meta = if meta_bits.is_empty() {
            String::new()
        } else {
            format!(" [{}]", meta_bits.join(", "))
        };
        println!("\n{title}{meta}");
        println!("  path  : {}", row.path);
        if !row.authors.is_empty() {
            println!("  authors: {}", row.authors);
        }
        if let Some(doi) = &row.doi {
            println!("  doi   : {doi}");
        }
        if let Some(arxiv) = &row.arxiv_id {
            println!("  arxiv : {arxiv}");
        }
    }
    Ok(())
}

// --- export / config ----------------------------------------------------

fn run_export(args: ExportArgs) -> Result<()> {
    let cfg = resolve_config(&args.common)?;
    let conn = db::open(&cfg.db_path)?;
    let rows = db::query(&conn, &Filter::default())?;

    let rendered = match args.format {
        ExportFormat::Json => serde_json::to_string_pretty(&rows)?,
        ExportFormat::Csv => to_csv(&rows),
    };

    match &args.out {
        Some(path) => {
            std::fs::write(path, rendered)
                .with_context(|| format!("writing {}", path.display()))?;
            println!("wrote {} row(s) to {}", rows.len(), path.display());
        }
        None => println!("{rendered}"),
    }
    Ok(())
}

fn run_config(args: ConfigArgs) -> Result<()> {
    if args.init {
        return run_config_init(&args);
    }

    let mut loaded = Config::load_detailed(args.common.config.as_deref())?;
    if let Some(db) = &args.common.db {
        loaded.config.db_path = db.clone();
        loaded.notes.push("db_path overridden by --db".to_string());
    }
    if loaded.config.papers_root.as_os_str().is_empty() {
        loaded
            .notes
            .push("papers_root is not set; scans will fail until it is configured".to_string());
    }

    if loaded.sources.is_empty() {
        eprintln!("config files: none loaded");
    } else {
        eprintln!("config files (low to high priority):");
        for source in &loaded.sources {
            eprintln!("  {}", source.display());
        }
    }
    for note in &loaded.notes {
        eprintln!("note: {note}");
    }

    if args.json {
        println!("{}", serde_json::to_string_pretty(&loaded.config)?);
    } else {
        println!("{}", toml::to_string_pretty(&loaded.config)?);
    }
    Ok(())
}

fn run_config_init(args: &ConfigArgs) -> Result<()> {
    let target = if args.global {
        biblio::config::user_config_path().ok_or_else(|| {
            anyhow::anyhow!("cannot determine the user config directory on this platform")
        })?
    } else {
        std::env::current_dir()?.join("config.toml")
    };
    biblio::config::write_template(&target, args.force)?;
    println!("wrote {}", target.display());
    Ok(())
}

fn run_stats(args: StatsArgs) -> Result<()> {
    let cfg = resolve_config(&args.common)?;
    let conn = db::open(&cfg.db_path)?;
    let stats = stats::collect(&conn, args.top)?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&stats)?);
    } else {
        print_stats_text(&stats);
    }
    Ok(())
}

fn run_tui(args: TuiArgs) -> Result<()> {
    if !std::io::stdout().is_terminal() {
        anyhow::bail!("`tui` requires an interactive terminal (stdout is not a TTY)");
    }
    let cfg = resolve_config(&args.common)?;
    let conn = db::open(&cfg.db_path)?;
    let stats = stats::collect(&conn, 20)?;
    tui::run(stats, cfg.db_path.display().to_string())
}

fn print_stats_text(s: &Stats) {
    println!("biblio catalog statistics");
    println!("  papers        : {}", fmt_int(s.total_papers));
    println!(
        "  authors       : {} (distinct)",
        fmt_int(s.distinct_authors)
    );
    println!("  DOIs          : {} (distinct)", fmt_int(s.distinct_dois));
    println!("  arXiv ids     : {} (distinct)", fmt_int(s.distinct_arxiv));
    println!("  pages         : {}", fmt_int(s.total_pages));
    println!("  size          : {}", human_bytes(s.total_bytes));

    println!("\nBy category");
    for c in &s.by_category {
        println!("  {:<12} {}", c.label, fmt_int(c.value));
    }

    println!("\nBy year");
    for c in &s.by_year {
        println!("  {:<6} {}", c.label, fmt_int(c.value));
    }

    println!("\nCoverage");
    for c in &s.coverage {
        let pct = if c.total > 0 {
            100.0 * c.present as f64 / c.total as f64
        } else {
            0.0
        };
        println!(
            "  {:<12} {:>5}/{:<5} {:>3.0}%",
            c.field, c.present, c.total, pct
        );
    }

    println!("\nTop authors");
    for (i, c) in s.top_authors.iter().enumerate() {
        println!("  {:>2}. {:<28} {}", i + 1, c.label, c.value);
    }

    println!("\nTop venues");
    for c in &s.top_venues {
        println!("  {:<20} {}", c.label, c.value);
    }

    println!("\nProvenance");
    for p in &s.provenance {
        println!("  {:<12} {:<10} {}", p.field, p.source, p.count);
    }

    println!("\nGrowth (papers first seen)");
    for c in &s.growth {
        println!("  {:<8} {}", c.label, c.value);
    }

    let a = &s.anomalies;
    println!("\nHealth");
    println!("  implausible years : {}", a.implausible_years.len());
    println!("  missing title+doi : {}", a.missing_title_doi);
    println!("  duplicate DOIs    : {}", a.duplicate_dois.len());
}

pub(crate) fn fmt_int(n: i64) -> String {
    let negative = n < 0;
    let digits = n.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    if negative { format!("-{out}") } else { out }
}

pub(crate) fn human_bytes(bytes: i64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", bytes, UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

const CSV_COLUMNS: [&str; 14] = [
    "path",
    "category",
    "subcategory",
    "filetype",
    "title",
    "authors",
    "publisher",
    "doi",
    "arxiv_id",
    "arxiv_doi",
    "venue",
    "year",
    "page_count",
    "keywords",
];

fn to_csv(rows: &[db::PaperRow]) -> String {
    let mut out = String::new();
    out.push_str(&CSV_COLUMNS.join(","));
    out.push('\n');
    for row in rows {
        let fields = [
            row.path.clone(),
            row.category.clone(),
            row.subcategory.clone().unwrap_or_default(),
            row.filetype.clone(),
            row.title.clone().unwrap_or_default(),
            row.authors.clone(),
            row.publisher.clone().unwrap_or_default(),
            row.doi.clone().unwrap_or_default(),
            row.arxiv_id.clone().unwrap_or_default(),
            row.arxiv_doi.clone().unwrap_or_default(),
            row.venue.clone().unwrap_or_default(),
            row.year.map(|y| y.to_string()).unwrap_or_default(),
            row.page_count.map(|p| p.to_string()).unwrap_or_default(),
            row.keywords.clone().unwrap_or_default(),
        ];
        let escaped: Vec<String> = fields.iter().map(|f| csv_escape(f)).collect();
        out.push_str(&escaped.join(","));
        out.push('\n');
    }
    out
}

fn csv_escape(field: &str) -> String {
    if field.contains(',') || field.contains('"') || field.contains('\n') {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_string()
    }
}
