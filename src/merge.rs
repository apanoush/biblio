//! Merge the metadata sources into a single [`Paper`].
//!
//! Priority for conflicting values is: embedded metadata, then file name.
//! Path-derived fields (category, file type) are always authoritative. Every
//! assigned field records its source and a confidence in [`Provenance`].

use crate::discover::DiscoveredFile;
use crate::filename::ParsedFilename;
use crate::metadata::Metadata;
use crate::model::{Author, FieldSource, Paper, Provenance};

/// A merged paper together with the provenance of each field.
#[derive(Debug, Clone, PartialEq)]
pub struct Extracted {
    pub paper: Paper,
    pub provenance: Vec<Provenance>,
}

/// Confidence constants, kept in one place for easy tuning.
const CONF_EMBEDDED: f64 = 0.95;
const CONF_FILENAME: f64 = 0.5;

/// Build a [`Paper`] from a discovered file and its extracted metadata.
pub fn build(
    file: &DiscoveredFile,
    embedded: Option<&Metadata>,
    parsed: &ParsedFilename,
) -> Extracted {
    let mut prov = Vec::new();

    let title = first_of(
        embedded
            .and_then(|m| m.title.clone())
            .map(|v| (v, FieldSource::Embedded)),
        parsed.title.clone().map(|v| (v, FieldSource::Filename)),
    );
    let (authors, authors_source) = if let Some(m) = embedded.filter(|m| !m.authors.is_empty()) {
        (m.authors.clone(), Some(FieldSource::Embedded))
    } else if !parsed.authors.is_empty() {
        (parsed.authors.clone(), Some(FieldSource::Filename))
    } else {
        (Vec::new(), None)
    };
    let doi = first_of(
        embedded
            .and_then(|m| m.doi.clone())
            .map(|v| (v, FieldSource::Embedded)),
        parsed.doi.clone().map(|v| (v, FieldSource::Filename)),
    );
    let publisher = embedded.and_then(|m| m.publisher.clone());
    let keywords = embedded.map(|m| m.keywords.clone()).unwrap_or_default();
    let page_count = embedded.and_then(|m| m.page_count);

    record_opt(&mut prov, "title", title.as_ref().map(|(_, s)| *s));
    record_src(&mut prov, "authors", authors_source);
    record_opt(&mut prov, "doi", doi.as_ref().map(|(_, s)| *s));
    record_src(
        &mut prov,
        "publisher",
        publisher.as_ref().map(|_| FieldSource::Embedded),
    );
    record_src(
        &mut prov,
        "keywords",
        (!keywords.is_empty()).then_some(FieldSource::Embedded),
    );
    record_src(
        &mut prov,
        "page_count",
        page_count.map(|_| FieldSource::Embedded),
    );
    record_src(
        &mut prov,
        "venue",
        parsed.venue.as_ref().map(|_| FieldSource::Filename),
    );
    record_src(
        &mut prov,
        "year",
        parsed.year.map(|_| FieldSource::Filename),
    );

    let (title, _) = unzip(title);
    let (doi, doi_source) = unzip(doi);

    // Derive the arXiv identifier and DOI.
    let (arxiv_id, arxiv_id_source) = derive_arxiv_id(parsed, doi.as_deref(), doi_source);
    let (arxiv_doi, arxiv_doi_source) = derive_arxiv_doi(arxiv_id.as_deref(), doi.as_deref());
    record_src(&mut prov, "arxiv_id", arxiv_id_source);
    record_src(&mut prov, "arxiv_doi", arxiv_doi_source);

    let author_structs = authors
        .into_iter()
        .enumerate()
        .map(|(i, name)| Author {
            position: i as u32,
            name,
        })
        .collect();

    // Path-derived fields are authoritative.
    record_src(&mut prov, "category", Some(FieldSource::Path));
    record_src(&mut prov, "filetype", Some(FieldSource::Path));

    let paper = Paper {
        path: file.rel_path.clone(),
        abs_path: file.abs_path.to_string_lossy().into_owned(),
        category: file.classification.category.clone(),
        subcategory: file.classification.subcategory.clone(),
        filetype: file.classification.filetype.clone(),
        title,
        authors: author_structs,
        publisher,
        doi,
        arxiv_id,
        arxiv_doi,
        venue: parsed.venue.clone(),
        year: parsed.year,
        page_count,
        keywords,
        size: file.size,
        mtime_ns: file.mtime_ns,
        sha256: file.sha256.clone(),
    };

    Extracted {
        paper,
        provenance: prov,
    }
}

/// Pick the first present option, preserving priority order.
fn first_of<T>(
    a: Option<(T, FieldSource)>,
    b: Option<(T, FieldSource)>,
) -> Option<(T, FieldSource)> {
    a.or(b)
}

/// Drop the value, keeping only its source.
fn unzip<T>(v: Option<(T, FieldSource)>) -> (Option<T>, Option<FieldSource>) {
    match v {
        Some((value, src)) => (Some(value), Some(src)),
        None => (None, None),
    }
}

fn record_opt(prov: &mut Vec<Provenance>, field: &str, source: Option<FieldSource>) {
    if let Some(source) = source {
        prov.push(Provenance {
            field: field.to_string(),
            source,
            confidence: confidence(source),
        });
    }
}

fn record_src(prov: &mut Vec<Provenance>, field: &str, source: Option<FieldSource>) {
    record_opt(prov, field, source);
}

fn confidence(source: FieldSource) -> f64 {
    match source {
        FieldSource::Embedded => CONF_EMBEDDED,
        FieldSource::Filename => CONF_FILENAME,
        FieldSource::Path => 1.0,
        FieldSource::Derived => 0.8,
    }
}

/// Determine the arXiv id from the file name or an arXiv DOI.
fn derive_arxiv_id(
    parsed: &ParsedFilename,
    doi: Option<&str>,
    doi_source: Option<FieldSource>,
) -> (Option<String>, Option<FieldSource>) {
    if let Some(id) = &parsed.arxiv_id {
        return (Some(id.clone()), Some(FieldSource::Filename));
    }
    if let Some(id) = doi.and_then(arxiv_id_from_doi) {
        let source = doi_source.unwrap_or(FieldSource::Derived);
        return (Some(id), Some(source));
    }
    (None, None)
}

/// `10.48550/arXiv.2406.12345` -> `2406.12345`.
pub fn arxiv_id_from_doi(doi: &str) -> Option<String> {
    doi.strip_prefix("10.48550/arXiv.")
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Build the canonical arXiv DOI from an arXiv id.
fn derive_arxiv_doi(
    arxiv_id: Option<&str>,
    doi: Option<&str>,
) -> (Option<String>, Option<FieldSource>) {
    if let Some(id) = arxiv_id {
        return (
            Some(format!("10.48550/arXiv.{id}")),
            Some(FieldSource::Derived),
        );
    }
    if let Some(d) = doi.filter(|d| d.starts_with("10.48550/arXiv.")) {
        return (Some(d.to_string()), Some(FieldSource::Embedded));
    }
    (None, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classify::Classification;

    fn discovered() -> DiscoveredFile {
        DiscoveredFile {
            rel_path: "preprints/pdfs/2506.07751v4.pdf".to_string(),
            abs_path: "/papers/preprints/pdfs/2506.07751v4.pdf".into(),
            size: 123,
            mtime_ns: 456,
            classification: Classification {
                category: "preprints".into(),
                subcategory: None,
                filetype: "pdf".into(),
            },
            sha256: None,
        }
    }

    #[test]
    fn arxiv_doi_is_derived_from_filename() {
        let parsed = crate::filename::parse("2506.07751v4");
        let e = build(&discovered(), None, &parsed);
        assert_eq!(e.paper.arxiv_id.as_deref(), Some("2506.07751"));
        assert_eq!(
            e.paper.arxiv_doi.as_deref(),
            Some("10.48550/arXiv.2506.07751")
        );
        assert_eq!(e.paper.category, "preprints");
    }

    #[test]
    fn embedded_title_beats_filename_title() {
        let parsed = crate::filename::parse("some_ugly_name");
        let md = Metadata {
            title: Some("Clean Title".into()),
            ..Metadata::default()
        };
        let e = build(&discovered(), Some(&md), &parsed);
        assert_eq!(e.paper.title.as_deref(), Some("Clean Title"));
        let title_prov = e.provenance.iter().find(|p| p.field == "title").unwrap();
        assert_eq!(title_prov.source, FieldSource::Embedded);
    }

    #[test]
    fn arxiv_id_from_doi_value() {
        assert_eq!(
            arxiv_id_from_doi("10.48550/arXiv.2406.12345").as_deref(),
            Some("2406.12345")
        );
        assert_eq!(arxiv_id_from_doi("10.1/x"), None);
    }
}
