//! Component records review (部件履历审查).
//!
//! Input is a records package (`aviator.records-review/v1`): documents, events
//! and every TSN/CSN/limit reading with the page and box it was read from.
//! Output is the review the desktop workbench renders: current TSN/CSN and
//! remaining life, the install/removal timeline, open items (missing and
//! suspicious records), two evidence chains (TSN, CSN) and a three-level part
//! tree from the per-type knowledge base.

mod engine;
mod knowledge;
mod lookup;
mod model;
mod routes;
mod value;

pub use engine::review;
pub use knowledge::build_tree;
pub use lookup::{LookupRequest, WorkbenchLookupCard, lookup_card, lookup_from_package};
pub use model::*;
pub use routes::records_review_routes;
