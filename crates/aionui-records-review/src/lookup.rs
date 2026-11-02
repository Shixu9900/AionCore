//! Small workbench card: one review result plus where to open it.

use serde::{Deserialize, Serialize};

use crate::engine::review;
use crate::model::{ComponentReview, PartNode, ReviewPackage, Verdict};

#[derive(Debug, Clone, Default, Deserialize)]
pub struct LookupRequest {
    #[serde(default)]
    pub query: String,
    pub package: ReviewPackage,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkbenchLookupCard {
    pub found: bool,
    pub query: String,
    pub part_number: String,
    pub serial_number: String,
    pub description: String,
    pub verdict: Option<String>,
    pub tsn: Option<String>,
    pub csn: Option<String>,
    pub remaining_cycles: Option<f64>,
    pub remaining_hours: Option<f64>,
    pub open_items: usize,
    pub page: String,
    pub focus: String,
    pub part: String,
    pub label: String,
}

pub fn lookup_from_package(package: &ReviewPackage, query: &str, today: chrono::NaiveDate) -> WorkbenchLookupCard {
    lookup_card(&review(package, today), query)
}

pub fn lookup_card(review: &ComponentReview, query: &str) -> WorkbenchLookupCard {
    let query = query.trim();
    let matched = match_part(review, query);
    let focus = suggest_focus(query);
    let part = matched
        .as_ref()
        .map(|node| preferred_part_key(node))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            if !review.component.serial_number.is_empty() {
                review.component.serial_number.clone()
            } else {
                review.component.part_number.clone()
            }
        });
    let found = query.is_empty()
        || matched.is_some()
        || component_matches(&review.component.part_number, query)
        || component_matches(&review.component.serial_number, query)
        || component_matches(&review.component.description, query);
    let description = matched
        .as_ref()
        .map(|node| node.label.clone())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| review.component.description.clone());
    let label = if part.is_empty() {
        description.clone()
    } else if description.is_empty() || description == part {
        part.clone()
    } else {
        format!("{part} · {description}")
    };
    WorkbenchLookupCard {
        found,
        query: query.to_string(),
        part_number: matched
            .as_ref()
            .map(|node| node.part_number.clone())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| review.component.part_number.clone()),
        serial_number: matched
            .as_ref()
            .map(|node| node.serial_number.clone())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| review.component.serial_number.clone()),
        description,
        verdict: Some(verdict_text(review.verdict)),
        tsn: review.life.tsn.as_ref().map(|value| value.display.clone()),
        csn: review.life.csn.as_ref().map(|value| value.display.clone()),
        remaining_cycles: review.life.remaining_cycles,
        remaining_hours: review.life.remaining_hours,
        open_items: review.open_items.len(),
        page: "llp".into(),
        focus: focus.into(),
        part: part.clone(),
        label,
    }
}

fn verdict_text(verdict: Verdict) -> String {
    match verdict {
        Verdict::Pass => "pass".into(),
        Verdict::Conditional => "conditional".into(),
        Verdict::Fail => "fail".into(),
    }
}

fn suggest_focus(query: &str) -> &'static str {
    let q = query.to_ascii_lowercase();
    if q.contains("open") || q.contains("missing") || q.contains("item") || q.contains("缺口") || q.contains("缺件")
    {
        "items"
    } else if q.contains("evidence") || q.contains("证据") {
        "evidence"
    } else if q.contains("life")
        || q.contains("寿命")
        || q.contains("remain")
        || q.contains("剩余")
        || q.contains("tsn")
        || q.contains("csn")
    {
        "life"
    } else {
        "summary"
    }
}

fn match_part<'a>(review: &'a ComponentReview, query: &str) -> Option<&'a PartNode> {
    let root = review.part_tree.root.as_ref()?;
    let mut nodes = Vec::new();
    collect_nodes(root, &mut nodes);
    if query.is_empty() {
        return nodes.iter().copied().find(|node| node.focus).or(Some(root));
    }
    nodes
        .iter()
        .copied()
        .find(|node| node_matches(node, query, true))
        .or_else(|| nodes.iter().copied().find(|node| node_matches(node, query, false)))
}

fn collect_nodes<'a>(node: &'a PartNode, out: &mut Vec<&'a PartNode>) {
    out.push(node);
    for child in &node.children {
        collect_nodes(child, out);
    }
}

fn node_matches(node: &PartNode, query: &str, exact: bool) -> bool {
    [
        node.id.as_str(),
        node.part_number.as_str(),
        node.serial_number.as_str(),
        node.label.as_str(),
    ]
    .into_iter()
    .any(|value| {
        if exact {
            eq_ignore_case(value, query) || query_has_token(query, value)
        } else {
            overlaps(value, query)
        }
    })
}

fn component_matches(value: &str, query: &str) -> bool {
    eq_ignore_case(value, query) || overlaps(value, query)
}

fn overlaps(value: &str, query: &str) -> bool {
    contains_ignore_case(value, query) || contains_ignore_case(query, value) || query_has_token(query, value)
}

fn query_has_token(query: &str, value: &str) -> bool {
    !value.is_empty()
        && query
            .split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '-')
            .any(|token| eq_ignore_case(token, value))
}

fn preferred_part_key(node: &PartNode) -> String {
    if !node.serial_number.is_empty() {
        node.serial_number.clone()
    } else if !node.part_number.is_empty() {
        node.part_number.clone()
    } else {
        node.id.clone()
    }
}

fn eq_ignore_case(left: &str, right: &str) -> bool {
    !left.is_empty() && left.eq_ignore_ascii_case(right)
}

fn contains_ignore_case(value: &str, query: &str) -> bool {
    !value.is_empty() && !query.is_empty() && value.to_ascii_lowercase().contains(&query.to_ascii_lowercase())
}
