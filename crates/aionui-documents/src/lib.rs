//! Document store, ingest, local MinerU lifecycle, dual-reader parse, and
//! project-filtered retrieval for the aviation workbench.

mod cascade;
mod golden;
mod ingest;
mod lifecycle;
mod observations;
mod provider;
mod quality;
mod queue;
mod retrieval;
mod routes;
mod routing;
mod store;
mod triage;
mod types;
mod worker;

pub use cascade::{PageParseOutcome, ParseOptions, ParseReport, parse_document, parse_page};
pub use golden::{GoldenField, field_exact_match, replay, seed_41744};
pub use ingest::{FsChange, IngestError, IngestLimits, add_file, apply_fs_event, scan_folder};
pub use lifecycle::{MinerUConfig, MinerULifecycle};
pub use observations::{annotate, package_from_store};
pub use provider::{CloudParseProvider, LocalDoclibProvider, ParseProvider, ParseRequest, ProviderError};
pub use queue::{LEASE_SECONDS, MAX_ATTEMPTS, ParseQueue, ParseTask, QueueProgress};
pub use retrieval::search_project;
pub use routes::{DocumentState, document_routes};
pub use routing::RoutingTable;
pub use store::DocumentStore;
pub use types::*;
pub use worker::run_queue;

/// Where the released test build stops a run and keeps the queue, so the user can
/// resume after upgrading. This is a pause, not a ceiling on file size: the
/// largest file in the 29,201-document AD/SB corpus is 17 pages, and a lease
/// return is tens of thousands of pages that the queue is built to carry.
pub const TRIAL_PAGES_PER_RUN: u32 = 500;
pub const WEB_PAGE_LIMIT: u32 = 200;
