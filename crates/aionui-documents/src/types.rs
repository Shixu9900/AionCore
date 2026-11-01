use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PageKind {
    Digital,
    CleanScan,
    PoorScan,
    Skewed,
    Office,
    Spreadsheet,
    Image,
    Unknown,
}

impl PageKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Digital => "digital",
            Self::CleanScan => "clean_scan",
            Self::PoorScan => "poor_scan",
            Self::Skewed => "skewed",
            Self::Office => "office",
            Self::Spreadsheet => "spreadsheet",
            Self::Image => "image",
            Self::Unknown => "unknown",
        }
    }

    pub fn parse(value: &str) -> Self {
        match value {
            "digital" => Self::Digital,
            "clean_scan" => Self::CleanScan,
            "poor_scan" => Self::PoorScan,
            "skewed" => Self::Skewed,
            "office" => Self::Office,
            "spreadsheet" => Self::Spreadsheet,
            "image" => Self::Image,
            _ => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    Basic,
    Standard,
    Advanced,
}

impl Tier {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Basic => "basic",
            Self::Standard => "standard",
            Self::Advanced => "advanced",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueCode {
    NoTextLayer,
    LowConfidence,
    TextLayerMismatch,
    TableRagged,
    Skewed,
    Handwriting,
    StampOverlap,
    IllegibleRegion,
    RenderFailed,
    PasswordProtected,
    ReaderDisagreement,
    PartialParse,
    ConstraintViolation,
    UnsupportedFormat,
    QuotaExceeded,
    EngineUnavailable,
}

impl IssueCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoTextLayer => "no_text_layer",
            Self::LowConfidence => "low_confidence",
            Self::TextLayerMismatch => "text_layer_mismatch",
            Self::TableRagged => "table_ragged",
            Self::Skewed => "skewed",
            Self::Handwriting => "handwriting",
            Self::StampOverlap => "stamp_overlap",
            Self::IllegibleRegion => "illegible_region",
            Self::RenderFailed => "render_failed",
            Self::PasswordProtected => "password_protected",
            Self::ReaderDisagreement => "reader_disagreement",
            Self::PartialParse => "partial_parse",
            Self::ConstraintViolation => "constraint_violation",
            Self::UnsupportedFormat => "unsupported_format",
            Self::QuotaExceeded => "quota_exceeded",
            Self::EngineUnavailable => "engine_unavailable",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentRow {
    pub doc_id: String,
    pub project_id: String,
    pub sha256: String,
    pub path: String,
    pub filename: String,
    pub page_count: u32,
    pub bytes: u64,
    pub added_at: String,
    pub deleted_at: Option<String>,
    pub superseded_by: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageRow {
    pub sha256: String,
    pub page_no: u32,
    pub kind: String,
    pub rotation: i32,
    pub skew_degrees: Option<f64>,
    pub text_chars: u32,
    pub image_coverage: f64,
    pub garbled_ratio: f64,
    pub status: String,
    pub confidence: Option<f64>,
    pub provider: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockRow {
    pub block_id: String,
    pub sha256: String,
    pub page_no: u32,
    pub label: Option<String>,
    pub bbox: Option<[f64; 4]>,
    pub text: String,
    pub html: Option<String>,
    pub confidence: Option<f64>,
    pub reading_order: Option<i32>,
    pub provider: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObservationRow {
    pub obs_id: String,
    pub project_id: String,
    pub sha256: String,
    pub page_no: u32,
    pub bbox: Option<[f64; 4]>,
    pub metric: String,
    pub raw: String,
    pub value: Option<f64>,
    pub confidence: Option<f64>,
    pub source: String,
    pub status: String,
    pub block_id: Option<String>,
    pub superseded_by: Option<String>,
    pub confirmed_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageIssue {
    pub issue_id: String,
    pub sha256: String,
    pub page_no: u32,
    pub bbox: Option<[f64; 4]>,
    pub code: String,
    pub severity: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldReading {
    pub metric: String,
    pub raw: String,
    pub value: Option<f64>,
    pub bbox: Option<[f64; 4]>,
    pub confidence: f64,
    pub provider: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReaderOutput {
    pub provider: String,
    pub tier: Option<String>,
    pub text: String,
    pub blocks: Vec<BlockRow>,
    pub fields: Vec<FieldReading>,
    pub confidence: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentReport {
    pub doc_id: String,
    pub sha256: String,
    pub filename: String,
    pub page_count: u32,
    pub passed: u32,
    pub doubtful: u32,
    pub failed: u32,
    pub issues: Vec<PageIssue>,
    pub engine: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    pub project_id: String,
    pub doc_id: String,
    pub sha256: String,
    pub filename: String,
    pub page_no: u32,
    pub block_id: String,
    pub snippet: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnnotateRequest {
    pub project_id: String,
    pub sha256: String,
    pub page_no: u32,
    pub bbox: Option<[f64; 4]>,
    pub metric: String,
    pub raw: String,
    pub value: Option<f64>,
}
