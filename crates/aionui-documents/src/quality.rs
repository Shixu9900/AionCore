use crate::types::{FieldReading, IssueCode, PageIssue, ReaderOutput};
use uuid::Uuid;

const PAGE_CONFIDENCE: f64 = 0.60;
const BLOCK_CONFIDENCE: f64 = 0.85;
const TEXT_AGREEMENT: f64 = 0.50;

pub fn page_issues(sha256: &str, page_no: u32, readers: &[ReaderOutput]) -> Vec<PageIssue> {
    let mut issues = Vec::new();
    if readers.is_empty() {
        issues.push(issue(sha256, page_no, IssueCode::PartialParse, "high", "No reader produced output."));
        return issues;
    }
    let avg = readers.iter().map(|r| r.confidence).sum::<f64>() / readers.len() as f64;
    if avg < PAGE_CONFIDENCE {
        issues.push(issue(
            sha256,
            page_no,
            IssueCode::LowConfidence,
            "medium",
            &format!("Page confidence {avg:.2} is below {PAGE_CONFIDENCE}."),
        ));
    }
    let blocks: Vec<_> = readers.iter().flat_map(|r| r.blocks.iter()).collect();
    if !blocks.is_empty() {
        let weak = blocks.iter().filter(|b| b.confidence.unwrap_or(0.0) < BLOCK_CONFIDENCE).count();
        if weak * 2 > blocks.len() {
            issues.push(issue(
                sha256,
                page_no,
                IssueCode::LowConfidence,
                "medium",
                "More than half of the blocks are below 0.85 confidence.",
            ));
        }
    }
    if let Some(text) = readers.iter().find(|r| r.provider == "text_layer")
        && let Some(other) = readers.iter().find(|r| r.provider != "text_layer")
    {
        let agree = token_overlap(&text.text, &other.text);
        if agree < TEXT_AGREEMENT {
            issues.push(issue(
                sha256,
                page_no,
                IssueCode::TextLayerMismatch,
                "high",
                &format!("Text layer and MinerU agree on only {agree:.0}% of tokens."),
            ));
        }
    }
    if ragged_table(readers) {
        issues.push(issue(
            sha256,
            page_no,
            IssueCode::TableRagged,
            "medium",
            "Table column counts are uneven across rows.",
        ));
    }
    issues.extend(field_disagreements(sha256, page_no, readers));
    issues
}

fn field_disagreements(sha256: &str, page_no: u32, readers: &[ReaderOutput]) -> Vec<PageIssue> {
    let mut issues = Vec::new();
    let metrics: Vec<String> = readers
        .iter()
        .flat_map(|r| r.fields.iter().map(|f| f.metric.clone()))
        .collect();
    for metric in unique(&metrics) {
        let values: Vec<&FieldReading> = readers
            .iter()
            .flat_map(|r| r.fields.iter())
            .filter(|f| f.metric == metric)
            .collect();
        if values.len() < 2 {
            if values.len() == 1 {
                issues.push(issue(
                    sha256,
                    page_no,
                    IssueCode::PartialParse,
                    "medium",
                    &format!("{metric} was read by only one reader."),
                ));
            }
            continue;
        }
        let first = normalize(&values[0].raw);
        if values.iter().any(|v| normalize(&v.raw) != first) {
            issues.push(issue(
                sha256,
                page_no,
                IssueCode::ReaderDisagreement,
                "high",
                &format!(
                    "{metric} disagrees: {}",
                    values
                        .iter()
                        .map(|v| format!("{}={}", v.provider, v.raw))
                        .collect::<Vec<_>>()
                        .join(" / ")
                ),
            ));
        }
    }
    issues
}

fn ragged_table(readers: &[ReaderOutput]) -> bool {
    readers.iter().any(|r| {
        let rows: Vec<&str> = r.text.lines().filter(|l| l.contains('|') || l.contains('\t')).collect();
        if rows.len() < 3 {
            return false;
        }
        let widths: Vec<usize> = rows.iter().map(|l| l.split(['|', '\t']).count()).collect();
        let min = *widths.iter().min().unwrap_or(&0);
        let max = *widths.iter().max().unwrap_or(&0);
        max > min + 1
    })
}

fn token_overlap(a: &str, b: &str) -> f64 {
    let left = tokens(a);
    let right = tokens(b);
    if left.is_empty() || right.is_empty() {
        return 0.0;
    }
    let hit = left.iter().filter(|t| right.contains(t)).count();
    hit as f64 / left.len() as f64
}

fn tokens(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|s| s.len() > 1)
        .map(|s| s.to_ascii_lowercase())
        .collect()
}

fn normalize(raw: &str) -> String {
    raw.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '.').collect::<String>().to_ascii_lowercase()
}

fn unique(items: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for item in items {
        if !out.contains(item) {
            out.push(item.clone());
        }
    }
    out
}

fn issue(sha256: &str, page_no: u32, code: IssueCode, severity: &str, detail: &str) -> PageIssue {
    PageIssue {
        issue_id: Uuid::now_v7().to_string(),
        sha256: sha256.to_string(),
        page_no,
        bbox: None,
        code: code.as_str().into(),
        severity: severity.into(),
        detail: detail.into(),
    }
}

pub fn page_status(issues: &[PageIssue]) -> &'static str {
    if issues.iter().any(|i| i.severity == "high") {
        "doubtful"
    } else if issues.is_empty() {
        "passed"
    } else {
        "doubtful"
    }
}
