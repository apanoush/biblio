//! Configuration loading.
//!
//! Configuration is layered. From lowest to highest priority:
//!
//! 1. built-in defaults
//! 2. a user-global file, in the platform config directory
//!    (`directories::ProjectDirs`, e.g. `~/.config/biblio/config.toml`)
//! 3. a project file, discovered by walking up from the current directory
//! 4. an explicit `--config` file (which disables project discovery)
//! 5. environment variables (`papers`/`PAPERS`, `BIBLIO_DB`)
//! 6. command-line flags, applied by the caller
//!
//! Each file is parsed into a [`PartialConfig`] and merged field by field, so
//! a higher layer only needs to set the keys it wants to change. The path
//! fields support `~`, `$VAR` and `${VAR}` expansion (via `shellexpand`),
//! resolved from the environment just before the environment overrides are
//! applied.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Template written by `biblio config --init`. Kept in sync with
/// `config.toml.example`.
pub const CONFIG_TEMPLATE: &str = include_str!("../config.toml.example");

/// Runtime configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Root of the paper tree. Read-only: never written by this tool.
    pub papers_root: PathBuf,
    /// SQLite database file.
    pub db_path: PathBuf,
    /// File extensions to consider, lowercase, without the dot.
    pub include_extensions: Vec<String>,
    /// Number of files passed to a single exiftool invocation.
    pub batch_size: usize,
    /// Compute a SHA-256 content hash for each file (slower).
    pub hash_contents: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            // Empty until set by a config file, the environment or a flag.
            papers_root: PathBuf::new(),
            db_path: PathBuf::from("data/biblio.db"),
            include_extensions: vec!["pdf".to_string(), "html".to_string(), "htm".to_string()],
            batch_size: 100,
            hash_contents: false,
        }
    }
}

/// The result of resolving configuration: the effective values, the files
/// that contributed to them, and any post-file overrides that were applied.
#[derive(Debug, Clone)]
pub struct LoadedConfig {
    /// The effective configuration.
    pub config: Config,
    /// Config files that were loaded, lowest priority first.
    pub sources: Vec<PathBuf>,
    /// Human-readable notes about environment/flag overrides.
    pub notes: Vec<String>,
}

/// Field-wise overlay parsed from a config file. Every field is optional so
/// that files can be layered over one another.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct PartialConfig {
    papers_root: Option<PathBuf>,
    db_path: Option<PathBuf>,
    include_extensions: Option<Vec<String>>,
    batch_size: Option<usize>,
    hash_contents: Option<bool>,
}

impl PartialConfig {
    /// Overlay the present fields onto `cfg`.
    fn apply_to(self, cfg: &mut Config) {
        if let Some(v) = self.papers_root {
            cfg.papers_root = v;
        }
        if let Some(v) = self.db_path {
            cfg.db_path = v;
        }
        if let Some(v) = self.include_extensions {
            cfg.include_extensions = v;
        }
        if let Some(v) = self.batch_size {
            cfg.batch_size = v;
        }
        if let Some(v) = self.hash_contents {
            cfg.hash_contents = v;
        }
    }
}

impl Config {
    /// Load the effective configuration. See the module docs for the layering
    /// rules.
    pub fn load(path: Option<&Path>) -> Result<Self> {
        Ok(Self::load_detailed(path)?.config)
    }

    /// Like [`Config::load`], but also returns which files were used and which
    /// overrides were applied. Used by the `config` subcommand.
    pub fn load_detailed(explicit: Option<&Path>) -> Result<LoadedConfig> {
        let mut cfg = Config::default();
        let mut sources = Vec::new();

        if let Some(user) = user_config_path()
            && user.is_file()
        {
            parse_file(&user)?.apply_to(&mut cfg);
            sources.push(user);
        }

        let project = match explicit {
            Some(p) => {
                if !p.is_file() {
                    anyhow::bail!("config file {} not found", p.display());
                }
                Some(p.to_path_buf())
            }
            None => discover_project_config(),
        };
        if let Some(p) = &project {
            parse_file(p)?.apply_to(&mut cfg);
            sources.push(p.clone());
        }

        // Expand `~` and `$VAR` in the path fields, using the environment.
        let papers_raw = cfg.papers_root.to_string_lossy().into_owned();
        cfg.papers_root = expand_path(&papers_raw)
            .with_context(|| format!("expanding `papers_root` = {papers_raw:?}"))?;
        let db_raw = cfg.db_path.to_string_lossy().into_owned();
        cfg.db_path =
            expand_path(&db_raw).with_context(|| format!("expanding `db_path` = {db_raw:?}"))?;

        let mut notes = Vec::new();
        if let Some(v) = env_var("papers").or_else(|| env_var("PAPERS")) {
            cfg.papers_root = PathBuf::from(v);
            notes.push("papers_root overridden by environment".to_string());
        }
        if let Some(v) = env_var("BIBLIO_DB") {
            cfg.db_path = PathBuf::from(v);
            notes.push("db_path overridden by environment".to_string());
        }

        Ok(LoadedConfig {
            config: cfg,
            sources,
            notes,
        })
    }

    /// Validate that the configuration is usable before scanning.
    pub fn validate(&self) -> Result<()> {
        if self.papers_root.as_os_str().is_empty() {
            anyhow::bail!(
                "no papers root configured: set `papers_root` in config.toml, \
                 the `papers` environment variable, or pass --root"
            );
        }
        if !self.papers_root.is_dir() {
            anyhow::bail!(
                "papers root {} is not a directory",
                self.papers_root.display()
            );
        }
        if self.batch_size == 0 {
            anyhow::bail!("batch_size must be greater than zero");
        }
        Ok(())
    }
}

/// Read and parse a config file into an overlay.
fn parse_file(path: &Path) -> Result<PartialConfig> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading config file {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("parsing config file {}", path.display()))
}

/// Environment lookup that treats empty values as unset.
fn env_var(name: &str) -> Option<std::ffi::OsString> {
    std::env::var_os(name).filter(|v| !v.is_empty())
}

// --- config discovery ---------------------------------------------------

/// User-global config path, from the platform config directory.
pub fn user_config_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "biblio")
        .map(|dirs| dirs.config_dir().join("config.toml"))
}

/// Write a template configuration file to `target`, creating parent
/// directories as needed. Refuses to overwrite unless `force` is set.
pub fn write_template(target: &Path, force: bool) -> Result<()> {
    if target.exists() && !force {
        anyhow::bail!(
            "{} already exists (use --force to overwrite)",
            target.display()
        );
    }
    if let Some(parent) = target.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(target, CONFIG_TEMPLATE)
        .with_context(|| format!("writing {}", target.display()))?;
    Ok(())
}

/// Project config discovered by walking up from the current directory.
fn discover_project_config() -> Option<PathBuf> {
    std::env::current_dir().ok().and_then(|d| discover_from(&d))
}

/// Walk `start` and its ancestors, returning the nearest `config.toml`.
fn discover_from(start: &Path) -> Option<PathBuf> {
    for dir in start.ancestors() {
        let candidate = dir.join("config.toml");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

// --- variable expansion -------------------------------------------------

/// Expand `~`, `$VAR` and `${VAR}` references using the process environment.
/// Referencing an unset variable is an error.
pub fn expand_path(raw: &str) -> Result<PathBuf> {
    let expanded = shellexpand::full(raw).map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(PathBuf::from(expanded.into_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Expand with a fake environment, mirroring `shellexpand::full` (missing
    /// variables are an error).
    fn expand_fake(input: &str, home: Option<&str>, vars: &[(&str, &str)]) -> Result<String> {
        let home = home.map(str::to_string);
        let context = |name: &str| -> Result<Option<String>, ()> {
            vars.iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| Some(v.to_string()))
                .ok_or(())
        };
        shellexpand::full_with_context(input, || home.clone(), context)
            .map(|c| c.into_owned())
            .map_err(|_| anyhow::anyhow!("lookup failed"))
    }

    #[test]
    fn expands_dollar_name() {
        assert_eq!(
            expand_fake("$FOO/bar", None, &[("FOO", "/x")]).unwrap(),
            "/x/bar"
        );
    }

    #[test]
    fn expands_braced_name() {
        assert_eq!(
            expand_fake("${FOO}/bar", None, &[("FOO", "/x")]).unwrap(),
            "/x/bar"
        );
    }

    #[test]
    fn expands_concatenated_names() {
        assert_eq!(
            expand_fake("$A$B", None, &[("A", "1"), ("B", "2")]).unwrap(),
            "12"
        );
    }

    #[test]
    fn expands_tilde() {
        assert_eq!(
            expand_fake("~/a", Some("/home/me"), &[]).unwrap(),
            "/home/me/a"
        );
    }

    #[test]
    fn unset_variable_is_an_error() {
        assert!(expand_fake("$MISSING/x", None, &[]).is_err());
        assert!(shellexpand::full("$BIBLIO_TEST_UNSET_VAR_XYZ").is_err());
    }

    #[test]
    fn relative_paths_are_unchanged() {
        assert_eq!(
            expand_fake("data/biblio.db", None, &[]).unwrap(),
            "data/biblio.db"
        );
    }

    #[test]
    fn user_config_path_is_under_biblio() {
        if let Some(path) = user_config_path() {
            assert!(
                path.ends_with("biblio/config.toml"),
                "unexpected path: {}",
                path.display()
            );
        }
    }

    #[test]
    fn discover_finds_nearest_ancestor_config() {
        let root = std::env::temp_dir().join(format!("biblio_cfg_{}", std::process::id()));
        let nested = root.join("a/b/c");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&nested).unwrap();

        // No config anywhere yet.
        assert_eq!(discover_from(&nested), None);

        // Far config only.
        std::fs::write(root.join("config.toml"), "batch_size = 1\n").unwrap();
        assert_eq!(discover_from(&nested), Some(root.join("config.toml")));

        // Nearer config wins.
        std::fs::write(root.join("a/b/config.toml"), "batch_size = 2\n").unwrap();
        assert_eq!(discover_from(&nested), Some(root.join("a/b/config.toml")));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn partial_config_overlays_only_present_fields() {
        let mut cfg = Config::default();
        let partial: PartialConfig = toml::from_str("batch_size = 5\n").unwrap();
        partial.apply_to(&mut cfg);
        assert_eq!(cfg.batch_size, 5);
        // Untouched fields keep their previous value.
        assert_eq!(cfg.db_path, PathBuf::from("data/biblio.db"));
    }

    #[test]
    fn partial_config_rejects_unknown_keys() {
        let parsed: Result<PartialConfig, _> = toml::from_str("nonsense = 1\n");
        assert!(parsed.is_err());
    }

    #[test]
    fn validate_requires_papers_root() {
        let cfg = Config::default();
        assert!(cfg.papers_root.as_os_str().is_empty());
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn template_is_valid_config() {
        let parsed: Result<PartialConfig, _> = toml::from_str(CONFIG_TEMPLATE);
        assert!(parsed.is_ok(), "template does not parse: {parsed:?}");
    }

    #[test]
    fn write_template_refuses_to_overwrite_without_force() {
        let dir = std::env::temp_dir().join(format!("biblio_init_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let target = dir.join("nested/config.toml");

        write_template(&target, false).unwrap();
        assert!(target.is_file());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), CONFIG_TEMPLATE);

        // A second write without --force must fail.
        assert!(write_template(&target, false).is_err());

        // With --force it succeeds.
        write_template(&target, true).unwrap();

        std::fs::remove_dir_all(&dir).ok();
    }
}
