//! Recover metadata from a file name when embedded metadata is missing.
//!
//! File names in the corpus follow a handful of conventions. We try a series
//! of ordered strategies and take the first that recognises the name. A final
//! heuristic treats a readable stem as a title.

use std::sync::LazyLock;

use regex::Regex;

/// Metadata recovered from a file name.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ParsedFilename {
    pub title: Option<String>,
    pub authors: Vec<String>,
    pub doi: Option<String>,
    pub arxiv_id: Option<String>,
    pub venue: Option<String>,
    pub year: Option<i32>,
}

static ARXIV: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?P<id>\d{4}\.\d{4,5})(?P<ver>v\d+)?$").unwrap());
static ACL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?P<year>\d{4})\.(?P<venue>[a-z][a-z0-9-]*)\.(?P<num>\d+)$").unwrap()
});
static NEURIPS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^NeurIPS-(?P<year>\d{4})-(?P<slug>.+)$").unwrap());
static ACM: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d{7}\.\d{7}$").unwrap());
static SPRINGER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^s\d{4,5}(-\d+)+$").unwrap());
static DOI_ENCODED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?P<prefix>10\.\d{4,})[_@/](?P<rest>.+)$").unwrap());
static YEAR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?P<year>(?:19|20)\d{2})").unwrap());

/// Parse `stem` (a file name without the extension) into metadata.
pub fn parse(stem: &str) -> ParsedFilename {
    let stem = stem.trim();
    if stem.is_empty() {
        return ParsedFilename::default();
    }

    if let Some(caps) = ARXIV.captures(stem) {
        return ParsedFilename {
            arxiv_id: Some(caps["id"].to_string()),
            ..ParsedFilename::default()
        };
    }

    if let Some(caps) = ACL.captures(stem) {
        return ParsedFilename {
            year: caps["year"].parse().ok(),
            venue: Some(normalize_venue(&caps["venue"])),
            ..ParsedFilename::default()
        };
    }

    if let Some(caps) = NEURIPS.captures(stem) {
        let slug = caps["slug"]
            .trim_end_matches("-Paper-Conference")
            .trim_end_matches("-Paper");
        return ParsedFilename {
            year: caps["year"].parse().ok(),
            venue: Some("NeurIPS".to_string()),
            title: Some(clean_slug(slug)),
            ..ParsedFilename::default()
        };
    }

    if ACM.is_match(stem) {
        return ParsedFilename {
            venue: Some("ACM".to_string()),
            ..ParsedFilename::default()
        };
    }

    if SPRINGER.is_match(stem) {
        return ParsedFilename {
            doi: Some(format!("10.1007/{stem}")),
            ..ParsedFilename::default()
        };
    }

    if let Some(caps) = DOI_ENCODED.captures(stem) {
        let rest = trim_doi_suffix(&caps["rest"]);
        return ParsedFilename {
            doi: Some(format!("{}/{}", &caps["prefix"], rest)),
            ..ParsedFilename::default()
        };
    }

    if let Some(parsed) = parse_author_year(stem) {
        return parsed;
    }

    ParsedFilename {
        title: fallback_title(stem),
        ..ParsedFilename::default()
    }
}

/// Extract an author/year/rest triple using the first four-digit year found.
fn parse_author_year(stem: &str) -> Option<ParsedFilename> {
    let m = YEAR.find(stem)?;
    let year: i32 = m.as_str().parse().ok()?;
    if !(1900..=2099).contains(&year) {
        return None;
    }
    let pre = stem[..m.start()].trim_matches(['-', '_', ' ']);
    let post = stem[m.end()..].trim_matches(['-', '_', ' ']);

    let mut parsed = ParsedFilename {
        year: Some(year),
        ..ParsedFilename::default()
    };

    if !pre.is_empty() && looks_like_author(pre) {
        parsed.authors = vec![clean_slug(pre)];
    }
    if !post.is_empty() {
        parsed.title = Some(clean_slug(post));
    }
    Some(parsed)
}

/// Heuristic: the prefix before a year looks like an author when it contains
/// letters, is short, and has no sentence-like structure.
fn looks_like_author(pre: &str) -> bool {
    let words = pre.split(['-', '_', ' ']).filter(|w| !w.is_empty()).count();
    pre.chars().any(char::is_alphabetic) && words <= 4 && pre.len() <= 40
}

/// Last resort: turn a readable stem into a title.
fn fallback_title(stem: &str) -> Option<String> {
    let has_separator = stem.contains('_') || stem.contains(' ');
    let letters = stem.chars().filter(|c| c.is_alphabetic()).count();
    let id_like = stem
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.');
    if has_separator || (letters >= 6 && !id_like) {
        Some(clean_title(stem))
    } else {
        None
    }
}

/// Replace underscores with spaces and collapse runs of whitespace. Hyphens
/// are preserved because they are often meaningful in prose titles.
pub fn clean_title(raw: &str) -> String {
    let replaced: String = raw
        .chars()
        .map(|c| if c == '_' { ' ' } else { c })
        .collect();
    replaced.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Like [`clean_title`], but also treats `-` as a word separator. Used for
/// slug-like names (`NeurIPS-...`, author-year file names) where dashes stand
/// in for spaces.
pub fn clean_slug(raw: &str) -> String {
    let replaced: String = raw
        .chars()
        .map(|c| if c == '_' || c == '-' { ' ' } else { c })
        .collect();
    replaced.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Map an ACL-style venue token to a readable venue name.
fn normalize_venue(token: &str) -> String {
    let lower = token.to_ascii_lowercase();
    if lower.contains("findings") {
        for base in ["acl", "emnlp", "naacl", "eacl"] {
            if lower.contains(base) {
                return format!("{} Findings", base.to_ascii_uppercase());
            }
        }
        return "Findings".to_string();
    }
    let head = lower
        .split(|c: char| c == '-' || c.is_ascii_digit())
        .next()
        .unwrap_or(&lower);
    match head {
        "acl" => "ACL".to_string(),
        "emnlp" => "EMNLP".to_string(),
        "naacl" => "NAACL".to_string(),
        "eacl" => "EACL".to_string(),
        "coling" => "COLING".to_string(),
        "bea" => "BEA".to_string(),
        "conll" => "CoNLL".to_string(),
        "semeval" => "SemEval".to_string(),
        other => other.to_ascii_uppercase(),
    }
}

/// Drop publisher-specific decorations from an encoded DOI.
fn trim_doi_suffix(rest: &str) -> String {
    for suffix in ["_previewpdf", "_preview", "-main", "_main"] {
        if let Some(stripped) = rest.strip_suffix(suffix) {
            return stripped.to_string();
        }
    }
    rest.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arxiv_id() {
        let p = parse("2506.07751v4");
        assert_eq!(p.arxiv_id.as_deref(), Some("2506.07751"));
        assert!(p.title.is_none());
    }

    #[test]
    fn acl_venue_and_year() {
        let p = parse("2025.emnlp-main.75");
        assert_eq!(p.year, Some(2025));
        assert_eq!(p.venue.as_deref(), Some("EMNLP"));
    }

    #[test]
    fn acl_findings() {
        let p = parse("2025.findings-acl.1180");
        assert_eq!(p.venue.as_deref(), Some("ACL Findings"));
    }

    #[test]
    fn neurips_title() {
        let p = parse(
            "NeurIPS-2018-learning-sparse-neural-networks-via-sensitivity-driven-regularization-Paper",
        );
        assert_eq!(p.year, Some(2018));
        assert_eq!(p.venue.as_deref(), Some("NeurIPS"));
        assert_eq!(
            p.title.as_deref(),
            Some("learning sparse neural networks via sensitivity driven regularization")
        );
    }

    #[test]
    fn acm_id() {
        let p = parse("3350546.3352513");
        assert_eq!(p.venue.as_deref(), Some("ACM"));
        assert!(p.title.is_none());
    }

    #[test]
    fn springer_id_becomes_doi() {
        let p = parse("s11097-018-9599-2");
        assert_eq!(p.doi.as_deref(), Some("10.1007/s11097-018-9599-2"));
    }

    #[test]
    fn author_year_simple() {
        let p = parse("delling2019");
        assert_eq!(p.year, Some(2019));
        assert_eq!(p.authors, vec!["delling"]);
    }

    #[test]
    fn author_year_with_title() {
        let p = parse("zhou-2012-the-state-of-precarious-work-in-china");
        assert_eq!(p.year, Some(2012));
        assert_eq!(p.authors, vec!["zhou"]);
        assert_eq!(
            p.title.as_deref(),
            Some("the state of precarious work in china")
        );
    }

    #[test]
    fn human_title_fallback() {
        let p = parse("Memory Attention");
        assert_eq!(p.title.as_deref(), Some("Memory Attention"));
    }

    #[test]
    fn underscore_title_fallback() {
        let p = parse("less_is_more");
        assert_eq!(p.title.as_deref(), Some("less is more"));
    }

    #[test]
    fn numeric_only_stem_has_no_title() {
        let p = parse("20-1364");
        assert!(p.title.is_none());
    }
}
