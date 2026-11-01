//! Document store, ingest, local MinerU lifecycle, dual-reader parse, and
//! project-filtered retrieval for the aviation workbench.

mod cascade;
mod golden;
mod ingest;
mod lifecycle;
mod observations;
mod provider;
mod quality;
mod retrieval;
mod routes;
mod routing;
mod store;
mod triage;
mod types;

pub use cascade::{ParseReport, parse_document};
pub use golden::{GoldenField, field_exact_match, replay, seed_41744};
pub use ingest::{FsChange, IngestError, IngestLimits, add_file, apply_fs_event, scan_folder};
pub use lifecycle::{MinerUConfig, MinerULifecycle};
pub use observations::{annotate, package_from_store};
pub use provider::{CloudParseProvider, LocalDoclibProvider, ParseProvider, ParseRequest, ProviderError};
pub use retrieval::search_project;
pub use routes::{DocumentState, document_routes};
pub use routing::RoutingTable;
pub use store::DocumentStore;
pub use types::*;

pub const DESKTOP_PAGE_LIMIT: u32 = 500;
pub const WEB_PAGE_LIMIT: u32 = 200;
