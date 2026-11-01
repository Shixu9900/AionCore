//! Input package (`aviator.records-review/v1`) and computed review result.
//!
//! The package is produced upstream (agent extraction or a converter) and only
//! carries what the documents say plus where they say it. Everything derived —
//! chain consensus, stints, remaining life, open items, the part tree — is
//! computed here so two runs over the same package always agree.

use serde::{Deserialize, Serialize};

pub const PACKAGE_SCHEMA: &str = "aviator.records-review/v1";
pub const RESULT_SCHEMA: &str = "aviator.records-review.result/v1";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReviewPackage {
    #[serde(default)]
    pub schema: String,
    pub component: ComponentIdentity,
    #[serde(default)]
    pub hierarchy: Vec<HierarchyEntry>,
    /// Serial numbers of peer parts installed in the same parent set.
    #[serde(default)]
    pub siblings: Vec<String>,
    #[serde(default)]
    pub cutoff_date: Option<String>,
    /// Date the review is performed; defaults to today. Drives the
    /// "no records after cutoff" open item.
    #[serde(default)]
    pub review_date: Option<String>,
    #[serde(default)]
    pub documents: Vec<DocumentEntry>,
    #[serde(default)]
    pub events: Vec<EventEntry>,
    #[serde(default)]
    pub observations: Vec<Observation>,
    #[serde(default)]
    pub open_items: Vec<DeclaredOpenItem>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ComponentIdentity {
    pub part_number: String,
    pub serial_number: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub ata: String,
    /// Host asset model used for knowledge-base lookup, e.g. `CFM56-7B26`.
    #[serde(default)]
    pub asset_model: String,
    #[serde(default)]
    pub thrust_rating: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HierarchyEntry {
    /// `system` | `module` | `part`
    pub level: String,
    #[serde(default)]
    pub part_number: String,
    #[serde(default)]
    pub serial_number: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub children_count: Option<u32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DocumentEntry {
    pub id: String,
    #[serde(default)]
    pub title: String,
    /// `movement_sheet` | `status_screen` | `release_certificate` |
    /// `shop_report` | `llp_sheet` | `logbook` | `statement` | `work_order` | `other`
    #[serde(default)]
    pub doc_type: String,
    /// Project-relative path of the source file.
    #[serde(default)]
    pub file: String,
    /// 1-based pages of `file` covered by this document.
    #[serde(default)]
    pub pages: Vec<u32>,
    #[serde(default)]
    pub party: String,
    #[serde(default)]
    pub date: String,
    #[serde(default)]
    pub signed: Option<bool>,
    /// `good` | `weak` | `failed`
    #[serde(default)]
    pub parse_quality: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HostInfo {
    #[serde(default)]
    pub esn: String,
    #[serde(default)]
    pub aircraft: String,
    #[serde(default)]
    pub registration: String,
    #[serde(default)]
    pub position: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EventEntry {
    pub id: String,
    pub date: String,
    /// `birth` | `install` | `removal` | `shop_visit` | `release` | `current`
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub host: HostInfo,
    #[serde(default)]
    pub operator: String,
    #[serde(default)]
    pub mro: String,
    #[serde(default)]
    pub documents: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Observation {
    pub id: String,
    pub doc: String,
    /// 1-based page number within the document's file.
    pub page: u32,
    /// Normalized `[x0, y0, x1, y1]` in 0..1 page coordinates.
    #[serde(default)]
    pub bbox: Option<[f64; 4]>,
    /// `tsn` | `csn` | `life_limit_cycles` | `life_limit_hours`
    pub metric: String,
    #[serde(default)]
    pub raw: String,
    /// Normalized value (decimal hours / cycles). Parsed from `raw` when absent.
    #[serde(default)]
    pub value: Option<f64>,
    #[serde(default)]
    pub event: Option<String>,
    #[serde(default)]
    pub as_of: Option<String>,
    /// `engine` | `human`. Human readings outrank engine readings.
    #[serde(default = "default_engine_source")]
    pub source: String,
}

fn default_engine_source() -> String {
    "engine".into()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeclaredOpenItem {
    #[serde(default)]
    pub id: String,
    /// `missing` | `suspicious`
    pub kind: String,
    #[serde(default)]
    pub severity: String,
    pub title: String,
    #[serde(default)]
    pub detail: String,
    #[serde(default)]
    pub request_from: String,
    #[serde(default)]
    pub documents: Vec<String>,
    #[serde(default)]
    pub observations: Vec<String>,
}

// ---------------------------------------------------------------------------
// Result
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentReview {
    pub schema: String,
    pub component: ComponentIdentity,
    pub cutoff_date: Option<String>,
    pub review_date: String,
    pub verdict: Verdict,
    pub life: LifeSummary,
    pub timeline: Vec<TimelineEntry>,
    pub stints: Vec<Stint>,
    pub open_items: Vec<OpenItem>,
    pub checks: Vec<Check>,
    pub evidence: EvidenceGraph,
    pub part_tree: PartTree,
    pub documents: Vec<DocumentEntry>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Pass,
    Conditional,
    Fail,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MetricValue {
    pub value: f64,
    pub display: String,
    pub as_of: String,
    pub observations: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LimitValue {
    pub value: f64,
    /// `documents` | `knowledge_base`
    pub source: String,
    pub detail: String,
    pub observations: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LifeSummary {
    pub tsn: Option<MetricValue>,
    pub csn: Option<MetricValue>,
    pub life_limit_cycles: Option<LimitValue>,
    pub life_limit_hours: Option<LimitValue>,
    pub remaining_cycles: Option<f64>,
    pub remaining_hours: Option<f64>,
    pub used_ratio: Option<f64>,
    pub formulas: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimelineEntry {
    pub id: String,
    pub date: String,
    pub kind: String,
    pub label: String,
    pub host: HostInfo,
    pub operator: String,
    pub mro: String,
    pub tsn: Option<f64>,
    pub csn: Option<f64>,
    pub documents: Vec<String>,
    pub observations: Vec<String>,
    pub flags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stint {
    pub install_event: String,
    pub removal_event: String,
    pub from: String,
    pub to: String,
    pub host: HostInfo,
    pub operator: String,
    pub used_tsn: Option<f64>,
    pub used_csn: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenItem {
    pub id: String,
    /// `missing` | `suspicious`
    pub kind: String,
    pub severity: Severity,
    pub title: String,
    pub detail: String,
    pub request_from: String,
    pub documents: Vec<String>,
    pub observations: Vec<String>,
    /// Rule code for engine findings, `declared` for package-supplied items.
    pub rule: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckResult {
    Pass,
    Warn,
    Fail,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Check {
    pub rule: String,
    pub result: CheckResult,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Consensus {
    Single,
    Consistent,
    Rounding,
    Conflict,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainPoint {
    pub id: String,
    pub date: String,
    pub event: Option<String>,
    pub value: f64,
    pub display: String,
    pub consensus: Consensus,
    pub observations: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceChain {
    pub metric: String,
    pub points: Vec<ChainPoint>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceCard {
    pub id: String,
    pub metric: String,
    pub point: Option<String>,
    pub doc: String,
    pub doc_title: String,
    pub doc_type: String,
    pub file: String,
    pub page: u32,
    pub bbox: Option<[f64; 4]>,
    pub raw: String,
    pub value: Option<f64>,
    pub date: String,
    pub signed: Option<bool>,
    pub parse_quality: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkKind {
    /// Next point in time along one chain.
    Sequence,
    /// Same quantity at the same point, from another document.
    Corroborates,
    /// Same point, values disagree beyond tolerance.
    Conflicts,
    /// TSN and CSN read from the same page.
    SamePage,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceLink {
    pub id: String,
    pub source: String,
    pub target: String,
    pub kind: LinkKind,
    pub label: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EvidenceGraph {
    pub chains: Vec<EvidenceChain>,
    pub cards: Vec<EvidenceCard>,
    pub links: Vec<EvidenceLink>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartNode {
    pub id: String,
    pub level: u8,
    pub label: String,
    pub part_number: String,
    pub serial_number: String,
    pub children_count: u32,
    pub life_limit_cycles: Option<f64>,
    pub focus: bool,
    pub children: Vec<PartNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartTree {
    /// `knowledge_base` | `documents` | `none`
    pub source: String,
    pub asset_model: String,
    pub root: Option<PartNode>,
}
