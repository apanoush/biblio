//! Walk the papers tree and collect candidate files.

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

use crate::classify::{self, Classification};

/// A file discovered on disk, before any metadata extraction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredFile {
    /// Path relative to the papers root.
    pub rel_path: String,
    /// Absolute path.
    pub abs_path: PathBuf,
    /// Size in bytes.
    pub size: i64,
    /// Modification time, nanoseconds since the Unix epoch.
    pub mtime_ns: i64,
    /// Position-derived classification.
    pub classification: Classification,
    /// Optional content hash, when `hash_contents` is enabled.
    pub sha256: Option<String>,
}

/// Walk `root`, returning one [`DiscoveredFile`] per matching file, sorted by
/// relative path for deterministic output. Symlinks are not followed.
pub fn discover(
    root: &Path,
    extensions: &[String],
    hash_contents: bool,
) -> Result<Vec<DiscoveredFile>> {
    let mut files = Vec::new();

    for entry in WalkDir::new(root).follow_links(false).into_iter() {
        let entry = match entry {
            Ok(e) => e,
            Err(err) => {
                tracing::warn!("skipping unreadable entry: {err}");
                continue;
            }
        };
        if !entry.file_type().is_file() {
            continue;
        }

        let abs_path = entry.path().to_path_buf();
        let ext = abs_path
            .extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_ascii_lowercase())
            .unwrap_or_default();
        if !extensions.iter().any(|e| e == &ext) {
            continue;
        }

        let metadata = entry
            .metadata()
            .with_context(|| format!("reading metadata for {}", abs_path.display()))?;
        let rel_path = abs_path
            .strip_prefix(root)
            .unwrap_or(&abs_path)
            .to_path_buf();
        let classification = classify::classify(&rel_path);

        let mtime_ns = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_nanos() as i64)
            .unwrap_or(0);

        let sha256 = if hash_contents {
            Some(hash_file(&abs_path)?)
        } else {
            None
        };

        files.push(DiscoveredFile {
            rel_path: rel_path.to_string_lossy().into_owned(),
            abs_path,
            size: metadata.len() as i64,
            mtime_ns,
            classification,
            sha256,
        });
    }

    files.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    Ok(files)
}

/// Stream a file through SHA-256 and return the lowercase hex digest.
pub fn hash_file(path: &Path) -> Result<String> {
    use std::io::Read;

    let mut file = std::fs::File::open(path)
        .with_context(|| format!("opening {} for hashing", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(to_hex(&hasher.finalize()))
}

fn to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}
