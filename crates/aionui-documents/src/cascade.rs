use std::path::Path;

use uuid::Uuid;

use crate::provider::{ParseProvider, ParseRequest, ProviderError};
use crate::quality::{page_issues, page_status};
use crate::routing::{ReaderKind, RoutingTable, mineru_tier};
use crate::store::DocumentStore;
use crate::triage::classify_path;
use crate::types::{
    BlockRow, DocumentReport, FieldReading, IssueCode, PageIssue, PageKind, PageRow, ReaderOutput, Tier,
};

#[derive(Debug, Clone, Copy, Default)]
pub struct ParseOptions {
    /// Re-read pages that already reached a terminal state. Off by default: a run
    /// that was interrupted should pick up where it stopped.
    pub force: bool,
    /// Pages to parse before stopping. `None` runs until the document is done.
    pub pages_per_run: Option<u32>,
}

#[derive(Debug)]
pub struct ParseReport {
    pub document: DocumentReport,
    /// Pages this call read.
    pub parsed: u32,
    /// Pages already finished by an earlier run.
    pub resumed: u32,
    /// Set when the run stopped with work still outstanding, either because the
    /// engine went away or because a page budget ran out. Everything finished is
    /// durable, so the next call continues from here.
    pub paused: Option<String>,
}

pub async fn parse_document<P: ParseProvider>(
    store: &DocumentStore,
    doc_id: &str,
    provider: &P,
    table: &RoutingTable,
    options: ParseOptions,
) -> Result<ParseReport, String> {
    let doc = store
        .find_by_id(doc_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "document not found".to_string())?;
    let path = Path::new(&doc.path);
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let (_, pages, mut page_texts) = classify_path(path, &bytes);
    // `classify_path` index-aligns `page_texts` with `pages`; pad defensively so a
    // short vector (should not happen, but must not panic) still yields empty text
    // rather than reusing a neighboring page's text.
    page_texts.resize(pages.len(), String::new());
    let done: Vec<u32> = store
        .list_pages(&doc.sha256)
        .await
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|p| matches!(p.status.as_str(), "passed" | "doubtful" | "failed"))
        .map(|p| p.page_no)
        .collect();
    let mut parsed = 0u32;
    let mut resumed = 0u32;
    let mut paused = None;
    for (mut page, page_text) in pages.into_iter().zip(page_texts) {
        page.sha256 = doc.sha256.clone();
        if !options.force && done.contains(&page.page_no) {
            resumed += 1;
            continue;
        }
        if options.pages_per_run.is_some_and(|budget| parsed >= budget) {
            paused = Some(format!(
                "This run read {parsed} pages and reached its budget. The remaining pages stay queued."
            ));
            break;
        }
        match read_and_record_page(store, &doc.sha256, path, page, &page_text, provider, table).await? {
            Recorded::Paused(reason) => {
                paused = Some(reason);
                break;
            }
            Recorded::Parsed => parsed += 1,
        }
    }
    Ok(ParseReport {
        document: store.report(&doc).await.map_err(|e| e.to_string())?,
        parsed,
        resumed,
        paused,
    })
}

enum Recorded {
    Parsed,
    Paused(String),
}

/// Outcome of parsing exactly one page. `Paused` means the engine was down, so
/// the page stays pending and the caller should hand it back rather than retry.
#[derive(Debug)]
pub enum PageParseOutcome {
    Done,
    Paused(String),
}

pub async fn parse_page<P: ParseProvider>(
    store: &DocumentStore,
    doc_id: &str,
    page_no: u32,
    provider: &P,
    table: &RoutingTable,
) -> Result<PageParseOutcome, String> {
    let doc = store
        .find_by_id(doc_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "document not found".to_string())?;
    let path = Path::new(&doc.path);
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let (_, pages, mut page_texts) = classify_path(path, &bytes);
    page_texts.resize(pages.len(), String::new());
    let Some((mut page, page_text)) = pages.into_iter().zip(page_texts).find(|(p, _)| p.page_no == page_no) else {
        return Err(format!("page {page_no} is not in the document"));
    };
    page.sha256 = doc.sha256.clone();
    match read_and_record_page(store, &doc.sha256, path, page, &page_text, provider, table).await? {
        Recorded::Paused(reason) => Ok(PageParseOutcome::Paused(reason)),
        Recorded::Parsed => Ok(PageParseOutcome::Done),
    }
}

async fn read_and_record_page<P: ParseProvider>(
    store: &DocumentStore,
    sha: &str,
    path: &Path,
    mut page: PageRow,
    page_text: &str,
    provider: &P,
    table: &RoutingTable,
) -> Result<Recorded, String> {
    let kind = PageKind::parse(&page.kind);
    let route = table.for_kind(kind);
    let mut readers = Vec::new();
    // Collected here, not inserted immediately: the unconditional `clear_issues`
    // call below would otherwise wipe out a reader failure the moment after it
    // was recorded, since `page_issues` only derives issues from successful
    // `readers` output and never resurfaces a failed reader's error.
    let mut reader_issues = Vec::new();
    for reader in &route.readers {
        match read_one(*reader, path, &page, page_text, provider).await {
            Ok(mut output) => {
                for block in &mut output.blocks {
                    if block.sha256.is_empty() {
                        block.sha256 = sha.to_string();
                    }
                    if block.page_no == 0 {
                        block.page_no = page.page_no;
                    }
                }
                readers.push(output);
            }
            // The engine being down says nothing about this page, so the page
            // goes back to pending and the run pauses. Marking it failed would
            // burn a good page, and returning Err would throw away every page
            // already read.
            Err(ProviderError::Unavailable | ProviderError::CloudDisabled) => {
                store
                    .insert_issue(&PageIssue {
                        issue_id: Uuid::now_v7().to_string(),
                        sha256: sha.to_string(),
                        page_no: page.page_no,
                        bbox: None,
                        code: IssueCode::EngineUnavailable.as_str().into(),
                        severity: "high".into(),
                        detail:
                            "The local parse engine is not running. The assistant will not send this page to the cloud."
                                .into(),
                    })
                    .await
                    .map_err(|e| e.to_string())?;
                page.status = "pending".into();
                store.upsert_page(&page).await.map_err(|e| e.to_string())?;
                return Ok(Recorded::Paused(
                    "The local parse engine is not running. Finished pages are saved and the rest stay queued.".into(),
                ));
            }
            Err(err) => {
                reader_issues.push(PageIssue {
                    issue_id: Uuid::now_v7().to_string(),
                    sha256: sha.to_string(),
                    page_no: page.page_no,
                    bbox: None,
                    code: IssueCode::RenderFailed.as_str().into(),
                    severity: "high".into(),
                    detail: err.to_string(),
                });
            }
        }
    }
    store.clear_issues(sha, page.page_no).await.map_err(|e| e.to_string())?;
    let mut issues = page_issues(sha, page.page_no, &readers);
    issues.extend(reader_issues);
    for issue in &issues {
        store.insert_issue(issue).await.map_err(|e| e.to_string())?;
    }
    let blocks: Vec<BlockRow> = readers.iter().flat_map(|r| r.blocks.clone()).collect();
    store
        .replace_blocks(sha, page.page_no, &blocks)
        .await
        .map_err(|e| e.to_string())?;
    page.status = page_status(&issues).into();
    page.confidence = readers.first().map(|r| r.confidence);
    page.provider = readers.first().map(|r| r.provider.clone());
    store.upsert_page(&page).await.map_err(|e| e.to_string())?;
    store
        .record_parse_run(
            sha,
            "local",
            route
                .readers
                .iter()
                .filter_map(|r| mineru_tier(*r).map(Tier::as_str))
                .next()
                .unwrap_or("none"),
            page.page_no,
            page.page_no,
            page.status.as_str(),
            None,
        )
        .await
        .map_err(|e| e.to_string())?;
    Ok(Recorded::Parsed)
}

async fn read_one<P: ParseProvider>(
    reader: ReaderKind,
    path: &Path,
    page: &PageRow,
    text_layer: &str,
    provider: &P,
) -> Result<ReaderOutput, ProviderError> {
    match reader {
        ReaderKind::TextLayer => Ok(ReaderOutput {
            provider: "text_layer".into(),
            tier: None,
            text: text_layer.to_string(),
            blocks: vec![BlockRow {
                block_id: format!("text-{}-{}", page.sha256, page.page_no),
                sha256: page.sha256.clone(),
                page_no: page.page_no,
                label: Some("text_layer".into()),
                bbox: None,
                text: text_layer.to_string(),
                html: None,
                confidence: Some(0.99),
                reading_order: Some(0),
                provider: "text_layer".into(),
            }],
            fields: extract_fields("text_layer", text_layer),
            confidence: 0.99,
        }),
        ReaderKind::OfficeXml | ReaderKind::Spreadsheet => Ok(ReaderOutput {
            provider: if reader == ReaderKind::OfficeXml {
                "office".into()
            } else {
                "spreadsheet".into()
            },
            tier: None,
            text: text_layer.to_string(),
            blocks: vec![BlockRow {
                block_id: format!("office-{}-{}", page.sha256, page.page_no),
                sha256: page.sha256.clone(),
                page_no: page.page_no,
                label: Some("structured".into()),
                bbox: None,
                text: text_layer.to_string(),
                html: None,
                confidence: Some(1.0),
                reading_order: Some(0),
                provider: "structured".into(),
            }],
            fields: extract_fields("structured", text_layer),
            confidence: 1.0,
        }),
        ReaderKind::MinerUBasic | ReaderKind::MinerUStandard | ReaderKind::MinerUAdvanced => {
            let tier = mineru_tier(reader).unwrap_or(Tier::Standard);
            provider
                .parse(&ParseRequest {
                    path: path.to_path_buf(),
                    tier,
                    page_from: page.page_no,
                    page_to: page.page_no,
                    force: false,
                    remote: false,
                })
                .await
        }
    }
}

fn extract_fields(provider: &str, text: &str) -> Vec<FieldReading> {
    let mut fields = Vec::new();
    for (metric, needle) in [("tsn", "TSN"), ("csn", "CSN"), ("part_number", "P/N"), ("serial_number", "S/N")] {
        if let Some(raw) = after_label(text, needle) {
            fields.push(FieldReading {
                metric: metric.into(),
                raw,
                value: None,
                bbox: None,
                confidence: 0.7,
                provider: provider.into(),
            });
        }
    }
    fields
}

fn after_label(text: &str, label: &str) -> Option<String> {
    let idx = text.find(label)?;
    let rest = text[idx + label.len()..].trim_start_matches([':', ' ', '\t']);
    let token = rest
        .split(|c: char| c.is_whitespace() || c == ',' || c == ';')
        .find(|s| !s.is_empty())?;
    Some(token.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '.' && c != ':').to_string())
}
