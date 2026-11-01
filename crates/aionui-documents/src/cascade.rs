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

#[derive(Debug)]
pub struct ParseReport {
    pub document: DocumentReport,
}

pub async fn parse_document<P: ParseProvider>(
    store: &DocumentStore,
    doc_id: &str,
    provider: &P,
    table: &RoutingTable,
) -> Result<ParseReport, String> {
    let doc = store
        .find_by_id(doc_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "document not found".to_string())?;
    let path = Path::new(&doc.path);
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let (_, pages, text_layer) = classify_path(path, &bytes);
    for mut page in pages {
        page.sha256 = doc.sha256.clone();
        let kind = PageKind::parse(&page.kind);
        let route = table.for_kind(kind);
        let mut readers = Vec::new();
        for reader in &route.readers {
            match read_one(*reader, path, &page, &text_layer, provider).await {
                Ok(mut output) => {
                    for block in &mut output.blocks {
                        if block.sha256.is_empty() {
                            block.sha256 = doc.sha256.clone();
                        }
                        if block.page_no == 0 {
                            block.page_no = page.page_no;
                        }
                    }
                    readers.push(output);
                }
                Err(ProviderError::Unavailable | ProviderError::CloudDisabled) => {
                    store
                        .insert_issue(&PageIssue {
                            issue_id: Uuid::now_v7().to_string(),
                            sha256: doc.sha256.clone(),
                            page_no: page.page_no,
                            bbox: None,
                            code: IssueCode::EngineUnavailable.as_str().into(),
                            severity: "high".into(),
                            detail: "The local parse engine is not running. The assistant will not send this page to the cloud.".into(),
                        })
                        .await
                        .map_err(|e| e.to_string())?;
                    page.status = "failed".into();
                    store.upsert_page(&page).await.map_err(|e| e.to_string())?;
                    return Err("local MinerU is not running".into());
                }
                Err(err) => {
                    store
                        .insert_issue(&PageIssue {
                            issue_id: Uuid::now_v7().to_string(),
                            sha256: doc.sha256.clone(),
                            page_no: page.page_no,
                            bbox: None,
                            code: IssueCode::RenderFailed.as_str().into(),
                            severity: "high".into(),
                            detail: err.to_string(),
                        })
                        .await
                        .map_err(|e| e.to_string())?;
                }
            }
        }
        store.clear_issues(&doc.sha256, page.page_no).await.map_err(|e| e.to_string())?;
        let issues = page_issues(&doc.sha256, page.page_no, &readers);
        for issue in &issues {
            store.insert_issue(issue).await.map_err(|e| e.to_string())?;
        }
        let blocks: Vec<BlockRow> = readers.iter().flat_map(|r| r.blocks.clone()).collect();
        store
            .replace_blocks(&doc.sha256, page.page_no, &blocks)
            .await
            .map_err(|e| e.to_string())?;
        page.status = page_status(&issues).into();
        page.confidence = readers.first().map(|r| r.confidence);
        page.provider = readers.first().map(|r| r.provider.clone());
        store.upsert_page(&page).await.map_err(|e| e.to_string())?;
        store
            .record_parse_run(
                &doc.sha256,
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
    }
    Ok(ParseReport {
        document: store.report(&doc).await.map_err(|e| e.to_string())?,
    })
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
