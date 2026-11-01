//! Per-type LLP knowledge base (ported from the leasewire registries) and the
//! three-level part tree: system → module/position → part.
//!
//! The registries were authored by different teams, so each loader tolerates
//! its own field spellings and normalizes into [`KbAsset`].

use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde_yaml::Value;

use crate::model::{ComponentIdentity, HierarchyEntry, PartNode, PartTree};

const REGISTRIES: &[(&str, &str)] = &[
    ("engine_cfm56_7b", include_str!("../knowledge/engine_cfm56_7b.yaml")),
    ("apu_gtcp_131_9b", include_str!("../knowledge/apu_gtcp_131_9b.yaml")),
    (
        "landing_gear_737ng",
        include_str!("../knowledge/landing_gear_737ng.yaml"),
    ),
    ("landing_gear_a320", include_str!("../knowledge/landing_gear_a320.yaml")),
];

#[derive(Debug, Clone)]
pub struct KbPart {
    pub part_number: String,
    pub base_part_number: String,
    pub designation: String,
    pub groups: Vec<String>,
    pub life_limit_cycles: Option<f64>,
    pub limits_by_rating: BTreeMap<String, f64>,
}

#[derive(Debug, Clone)]
pub struct KbAsset {
    pub key: String,
    pub asset_model: String,
    pub variants: Vec<String>,
    pub parts: Vec<KbPart>,
}

pub fn registry() -> &'static [KbAsset] {
    static CACHE: OnceLock<Vec<KbAsset>> = OnceLock::new();
    CACHE.get_or_init(|| {
        REGISTRIES
            .iter()
            .filter_map(|(key, text)| serde_yaml::from_str::<Value>(text).ok().map(|v| load_asset(key, &v)))
            .collect()
    })
}

fn s(v: &Value, key: &str) -> String {
    match v.get(key) {
        Some(Value::String(x)) => x.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

fn num(v: Option<&Value>) -> Option<f64> {
    match v? {
        Value::Number(n) => n.as_f64(),
        Value::String(x) => x.replace(',', "").parse().ok(),
        _ => None,
    }
}

fn load_asset(key: &str, doc: &Value) -> KbAsset {
    let default_limit = num(doc.get("default_llp_life_limit_cycles"));
    let variants = doc
        .get("asset_variants")
        .and_then(Value::as_sequence)
        .map(|seq| seq.iter().filter_map(Value::as_str).map(str::to_string).collect())
        .unwrap_or_default();
    let parts = doc
        .get("llp_parts")
        .and_then(Value::as_sequence)
        .map(|seq| seq.iter().map(|p| load_part(p, default_limit)).collect())
        .unwrap_or_default();
    KbAsset {
        key: key.to_string(),
        asset_model: s(doc, "asset_model"),
        variants,
        parts,
    }
}

fn load_part(p: &Value, default_limit: Option<f64>) -> KbPart {
    let designation = [s(p, "designation"), s(p, "component_name")]
        .into_iter()
        .find(|x| !x.is_empty())
        .or_else(|| {
            p.get("descriptions")
                .and_then(Value::as_sequence)
                .and_then(|seq| seq.first())
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_default();
    let mut groups: Vec<String> = Vec::new();
    let module_group = s(p, "module_group");
    if !module_group.is_empty() {
        groups.push(module_group);
    }
    if let Some(seq) = p.get("gear_positions").and_then(Value::as_sequence) {
        groups.extend(seq.iter().filter_map(Value::as_str).map(str::to_string));
    }
    if groups.is_empty() {
        groups.push("other".to_string());
    }
    let mut limits_by_rating = BTreeMap::new();
    if let Some(map) = p.get("life_limit_cycles_by_thrust_rating").and_then(Value::as_mapping) {
        for (k, v) in map {
            if let (Some(k), Some(v)) = (k.as_str(), num(Some(v))) {
                limits_by_rating.insert(k.to_string(), v);
            }
        }
    }
    let scrap = p
        .get("limit_categories")
        .and_then(|c| c.get("scrap"))
        .and_then(|c| c.get("life_limit_cycles"));
    let life_limit_cycles = num(p.get("life_limit_cycles")).or_else(|| num(scrap)).or(default_limit);
    let part_number = s(p, "part_number");
    let base = s(p, "base_part_number");
    KbPart {
        base_part_number: if base.is_empty() { part_number.clone() } else { base },
        part_number,
        designation,
        groups,
        life_limit_cycles,
        limits_by_rating,
    }
}

fn norm_pn(pn: &str) -> String {
    pn.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_uppercase()
}

impl KbPart {
    fn matches(&self, pn: &str) -> bool {
        let target = norm_pn(pn);
        if target.is_empty() {
            return false;
        }
        let full = norm_pn(&self.part_number);
        let base = norm_pn(&self.base_part_number);
        target == full || target == base || (!base.is_empty() && target.starts_with(&base))
    }

    /// Cycle limit for the host thrust rating; the most restrictive rating when
    /// the rating is unknown.
    pub fn limit_for(&self, rating: &str) -> Option<(f64, String)> {
        if self.limits_by_rating.is_empty() {
            return self.life_limit_cycles.map(|v| (v, String::new()));
        }
        if !rating.is_empty()
            && let Some(v) = self.limits_by_rating.get(rating)
        {
            return Some((*v, format!("推力 {rating}")));
        }
        self.limits_by_rating
            .iter()
            .min_by(|a, b| a.1.total_cmp(b.1))
            .map(|(k, v)| (*v, format!("推力未知，取最严 {k}")))
    }
}

impl KbAsset {
    fn matches_model(&self, model: &str) -> bool {
        let target = norm_pn(model);
        if target.is_empty() {
            return false;
        }
        let own = norm_pn(&self.asset_model);
        (!own.is_empty() && target.contains(&own)) || self.variants.iter().any(|v| norm_pn(v) == target)
    }
}

pub struct KbMatch<'a> {
    pub asset: &'a KbAsset,
    pub part: Option<&'a KbPart>,
}

pub fn lookup(component: &ComponentIdentity) -> Option<KbMatch<'static>> {
    let assets = registry();
    let model_hit = |a: &&KbAsset| component.asset_model.is_empty() || a.matches_model(&component.asset_model);
    for asset in assets.iter().filter(model_hit) {
        if let Some(part) = asset.parts.iter().find(|p| p.matches(&component.part_number)) {
            return Some(KbMatch {
                asset,
                part: Some(part),
            });
        }
    }
    assets
        .iter()
        .find(|a| a.matches_model(&component.asset_model))
        .map(|asset| KbMatch { asset, part: None })
}

/// `CFM56-7B26` → `7B26`, used when the package names the model but not the rating.
pub fn rating_from_model(model: &str) -> String {
    let upper = model.to_ascii_uppercase();
    upper
        .find("7B")
        .map(|i| upper[i..].chars().take(4).collect())
        .unwrap_or_default()
}

fn humanize(group: &str) -> String {
    match group {
        "fan" => "Fan 风扇".into(),
        "fan_and_booster" => "Fan & Booster 风扇/增压级".into(),
        "high_pressure_compressor" => "HPC 高压压气机".into(),
        "high_pressure_turbine" => "HPT 高压涡轮".into(),
        "low_pressure_turbine" => "LPT 低压涡轮".into(),
        "apu_core" => "APU Core 核心机".into(),
        "lh_mlg" => "LH MLG 左主起".into(),
        "rh_mlg" => "RH MLG 右主起".into(),
        "nlg" => "NLG 前起".into(),
        other => other.replace('_', " "),
    }
}

pub fn build_tree(component: &ComponentIdentity, hierarchy: &[HierarchyEntry], siblings: &[String]) -> PartTree {
    if let Some(hit) = lookup(component)
        && hit.part.is_some()
    {
        return kb_tree(hit, component);
    }
    if !hierarchy.is_empty() {
        return documents_tree(component, hierarchy, siblings);
    }
    match lookup(component) {
        Some(hit) => kb_tree(hit, component),
        None => PartTree {
            source: "none".into(),
            asset_model: component.asset_model.clone(),
            root: None,
        },
    }
}

fn kb_tree(hit: KbMatch<'_>, component: &ComponentIdentity) -> PartTree {
    let rating = if component.thrust_rating.is_empty() {
        rating_from_model(&component.asset_model)
    } else {
        component.thrust_rating.clone()
    };
    let focus_pn = hit.part.map(|p| p.part_number.clone()).unwrap_or_default();
    let mut groups: BTreeMap<String, Vec<&KbPart>> = BTreeMap::new();
    for part in &hit.asset.parts {
        for g in &part.groups {
            groups.entry(g.clone()).or_default().push(part);
        }
    }
    let modules: Vec<PartNode> = groups
        .into_iter()
        .map(|(group, parts)| {
            let children: Vec<PartNode> = parts
                .iter()
                .map(|p| {
                    let focus = p.part_number == focus_pn;
                    PartNode {
                        id: format!("{}:{}:{}", hit.asset.key, group, p.part_number),
                        level: 3,
                        label: p.designation.clone(),
                        part_number: p.part_number.clone(),
                        serial_number: if focus {
                            component.serial_number.clone()
                        } else {
                            String::new()
                        },
                        children_count: 0,
                        life_limit_cycles: p.limit_for(&rating).map(|(v, _)| v),
                        focus,
                        children: Vec::new(),
                    }
                })
                .collect();
            let focus = children.iter().any(|c| c.focus);
            PartNode {
                id: format!("{}:{}", hit.asset.key, group),
                level: 2,
                label: humanize(&group),
                part_number: String::new(),
                serial_number: String::new(),
                children_count: children.len() as u32,
                life_limit_cycles: None,
                focus,
                children,
            }
        })
        .collect();
    let model = if component.asset_model.is_empty() {
        hit.asset.asset_model.clone()
    } else {
        component.asset_model.clone()
    };
    PartTree {
        source: "knowledge_base".into(),
        asset_model: hit.asset.asset_model.clone(),
        root: Some(PartNode {
            id: hit.asset.key.clone(),
            level: 1,
            label: model,
            part_number: String::new(),
            serial_number: String::new(),
            children_count: modules.len() as u32,
            life_limit_cycles: None,
            focus: false,
            children: modules,
        }),
    }
}

/// Tree from the host chain stated in the documents (system → module → part),
/// used when the knowledge base has no entry for this type.
fn documents_tree(component: &ComponentIdentity, hierarchy: &[HierarchyEntry], siblings: &[String]) -> PartTree {
    let mut chain: Vec<&HierarchyEntry> = hierarchy.iter().take(3).collect();
    let focus_in_chain = chain
        .iter()
        .any(|h| h.serial_number == component.serial_number && h.part_number == component.part_number);
    let own = HierarchyEntry {
        level: "part".into(),
        part_number: component.part_number.clone(),
        serial_number: component.serial_number.clone(),
        description: component.description.clone(),
        children_count: Some(0),
    };
    if !focus_in_chain {
        if chain.len() == 3 {
            chain.pop();
        }
        chain.push(&own);
    }
    let depth = chain.len();
    let mut node: Option<PartNode> = None;
    for (i, entry) in chain.iter().enumerate().rev() {
        let level = (i + 1) as u8;
        let is_focus = i == depth - 1;
        let mut children: Vec<PartNode> = node.take().into_iter().collect();
        if i == depth.saturating_sub(2) {
            for sn in siblings.iter().filter(|sn| **sn != component.serial_number) {
                children.push(PartNode {
                    id: format!("sib:{sn}"),
                    level: level + 1,
                    label: component.description.clone(),
                    part_number: component.part_number.clone(),
                    serial_number: sn.clone(),
                    children_count: 0,
                    life_limit_cycles: None,
                    focus: false,
                    children: Vec::new(),
                });
            }
        }
        let count = entry.children_count.unwrap_or(children.len() as u32);
        node = Some(PartNode {
            id: format!("doc:{}:{}", entry.part_number, entry.serial_number),
            level,
            label: entry.description.clone(),
            part_number: entry.part_number.clone(),
            serial_number: entry.serial_number.clone(),
            children_count: count.max(children.len() as u32),
            life_limit_cycles: None,
            focus: is_focus,
            children,
        });
    }
    PartTree {
        source: "documents".into(),
        asset_model: component.asset_model.clone(),
        root: node,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_registries_load() {
        let reg = registry();
        assert_eq!(reg.len(), 4);
        assert!(reg.iter().all(|a| !a.parts.is_empty()));
    }

    #[test]
    fn cfm_part_resolves_three_levels() {
        let component = ComponentIdentity {
            part_number: "340-000-420-0".into(),
            serial_number: "ABC123".into(),
            asset_model: "CFM56-7B26".into(),
            ..Default::default()
        };
        let tree = build_tree(&component, &[], &[]);
        assert_eq!(tree.source, "knowledge_base");
        let root = tree.root.unwrap();
        let module = root.children.iter().find(|m| m.focus).unwrap();
        let part = module.children.iter().find(|p| p.focus).unwrap();
        assert_eq!(part.serial_number, "ABC123");
        assert_eq!(part.life_limit_cycles, Some(30000.0));
    }
}
