use serde::{Deserialize, Serialize};

use crate::types::{PageKind, Tier};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReaderKind {
    TextLayer,
    MinerUStandard,
    MinerUAdvanced,
    MinerUBasic,
    OfficeXml,
    Spreadsheet,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Route {
    pub kind: PageKind,
    pub readers: Vec<ReaderKind>,
    pub preprocess: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingTable {
    pub routes: Vec<Route>,
}

impl Default for RoutingTable {
    fn default() -> Self {
        Self {
            routes: vec![
                Route {
                    kind: PageKind::Digital,
                    readers: vec![ReaderKind::TextLayer, ReaderKind::MinerUStandard],
                    preprocess: false,
                },
                Route {
                    kind: PageKind::CleanScan,
                    readers: vec![ReaderKind::MinerUAdvanced, ReaderKind::MinerUBasic],
                    preprocess: false,
                },
                Route {
                    kind: PageKind::PoorScan,
                    readers: vec![ReaderKind::MinerUAdvanced, ReaderKind::MinerUBasic],
                    preprocess: true,
                },
                Route {
                    kind: PageKind::Skewed,
                    readers: vec![ReaderKind::MinerUAdvanced, ReaderKind::MinerUBasic],
                    preprocess: true,
                },
                Route {
                    kind: PageKind::Image,
                    readers: vec![ReaderKind::MinerUAdvanced, ReaderKind::MinerUBasic],
                    preprocess: false,
                },
                Route {
                    kind: PageKind::Office,
                    readers: vec![ReaderKind::OfficeXml],
                    preprocess: false,
                },
                Route {
                    kind: PageKind::Spreadsheet,
                    readers: vec![ReaderKind::Spreadsheet],
                    preprocess: false,
                },
            ],
        }
    }
}

impl RoutingTable {
    pub fn for_kind(&self, kind: PageKind) -> Route {
        self.routes
            .iter()
            .find(|r| r.kind == kind)
            .cloned()
            .unwrap_or(Route {
                kind,
                readers: vec![ReaderKind::MinerUStandard],
                preprocess: false,
            })
    }
}

pub fn mineru_tier(reader: ReaderKind) -> Option<Tier> {
    match reader {
        ReaderKind::MinerUBasic => Some(Tier::Basic),
        ReaderKind::MinerUStandard => Some(Tier::Standard),
        ReaderKind::MinerUAdvanced => Some(Tier::Advanced),
        ReaderKind::TextLayer | ReaderKind::OfficeXml | ReaderKind::Spreadsheet => None,
    }
}
