//! Classify a paper by its position in the tree.
//!
//! The tree is organised as `<category>/<optional subfolder>/<file>`, where
//! the category is one of `journal`, `conference`, `preprints`, `workshops`,
//! `other`, `old`. A `pdfs/` intermediate folder is common and carries no
//! meaning, so it is not treated as a subcategory.

use std::path::Path;

/// A file's position-derived classification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Classification {
    /// Top-level category folder, lowercased.
    pub category: String,
    /// Second-level folder, unless it is the generic `pdfs` container.
    pub subcategory: Option<String>,
    /// File extension, lowercased.
    pub filetype: String,
}

/// Infer the classification of `rel_path` (a path relative to the papers root).
///
/// Missing path components degrade gracefully to `"unknown"`.
pub fn classify(rel_path: &Path) -> Classification {
    let components: Vec<String> = rel_path
        .components()
        .filter_map(|c| c.as_os_str().to_str())
        .map(|s| s.to_string())
        .collect();

    let category = components
        .first()
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_else(|| "unknown".to_string());

    // A subcategory exists only when there is at least one directory between
    // the category and the file, and it is not the generic `pdfs` folder.
    let subcategory = if components.len() >= 3 {
        let candidate = components[1].to_ascii_lowercase();
        if candidate == "pdfs" {
            None
        } else {
            Some(candidate)
        }
    } else {
        None
    };

    let filetype = rel_path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_else(|| "unknown".to_string());

    Classification {
        category,
        subcategory,
        filetype,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classify_str(p: &str) -> Classification {
        classify(Path::new(p))
    }

    #[test]
    fn standard_pdfs_folder_has_no_subcategory() {
        let c = classify_str("journal/pdfs/foo.pdf");
        assert_eq!(c.category, "journal");
        assert_eq!(c.subcategory, None);
        assert_eq!(c.filetype, "pdf");
    }

    #[test]
    fn named_subfolder_is_kept() {
        let c = classify_str("old/chuv/foo.PDF");
        assert_eq!(c.category, "old");
        assert_eq!(c.subcategory.as_deref(), Some("chuv"));
        assert_eq!(c.filetype, "pdf");
    }

    #[test]
    fn file_directly_in_category() {
        let c = classify_str("preprints/2509.24317v1.pdf");
        assert_eq!(c.category, "preprints");
        assert_eq!(c.subcategory, None);
    }

    #[test]
    fn missing_extension_is_unknown() {
        let c = classify_str("other/Makefile");
        assert_eq!(c.category, "other");
        assert_eq!(c.filetype, "unknown");
    }
}
