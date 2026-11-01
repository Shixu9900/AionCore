//! Deterministic records review: evidence chains → life → timeline → open items.
//!
//! Rules follow `knowledge/common/btb_calculation.md` and `btb_audit_rules.md`
//! from the leasewire project: TSN/CSN must be non-decreasing, an install
//! continues the previous removal, a stint's usage is removal − install, a 0/0
//! first install is an acceptable baseline when the birth document is missing.

use std::collections::{BTreeMap, HashMap};

use chrono::{Datelike, NaiveDate};

use crate::knowledge::{self, rating_from_model};
use crate::model::*;
use crate::value::{format_cycles, format_hours, normalize_date, parse_cycles, parse_date, parse_hours};

/// TSN readings within this many hours are the same reading in another unit
/// convention (HH:MM vs decimal, rounding to whole hours).
const TSN_ROUNDING_HOURS: f64 = 1.0;
const GAP_YEARS: i64 = 3;
const STALE_CUTOFF_DAYS: i64 = 365;

struct Obs<'a> {
    raw: &'a Observation,
    value: Option<f64>,
    date: String,
    doc: Option<&'a DocumentEntry>,
}

impl Obs<'_> {
    /// Minute-resolution hour readings (`HH:MM`, `HHHH.MM`) outrank one-decimal
    /// approximations of the same reading.
    fn precision(&self) -> u8 {
        let raw = self.raw.raw.replace(',', "");
        if raw.contains(':') {
            return 2;
        }
        match raw.split_once('.') {
            Some((_, frac)) if frac.len() >= 2 => 2,
            Some(_) => 1,
            None => 0,
        }
    }

    fn is_human(&self) -> bool {
        self.raw.source == "human"
    }
}

pub fn review(pkg: &ReviewPackage, today: NaiveDate) -> ComponentReview {
    let docs: HashMap<&str, &DocumentEntry> = pkg.documents.iter().map(|d| (d.id.as_str(), d)).collect();
    let mut events: Vec<&EventEntry> = pkg.events.iter().collect();
    events.sort_by_key(|e| date_key(&e.date));
    let event_by_id: HashMap<&str, &EventEntry> = events.iter().map(|e| (e.id.as_str(), *e)).collect();
    let event_rank: HashMap<&str, usize> = events.iter().enumerate().map(|(i, e)| (e.id.as_str(), i)).collect();

    let observations: Vec<Obs> = pkg
        .observations
        .iter()
        .map(|o| {
            let value = o.value.or_else(|| match o.metric.as_str() {
                "tsn" | "life_limit_hours" => parse_hours(&o.raw),
                _ => parse_cycles(&o.raw),
            });
            let date = o
                .as_of
                .clone()
                .or_else(|| {
                    o.event
                        .as_deref()
                        .and_then(|id| event_by_id.get(id))
                        .map(|e| e.date.clone())
                })
                .map(|d| normalize_date(&d))
                .unwrap_or_default();
            Obs {
                raw: o,
                value,
                date,
                doc: docs.get(o.doc.as_str()).copied(),
            }
        })
        .collect();

    let cutoff = pkg.cutoff_date.as_deref().map(normalize_date);
    let review_date = pkg
        .review_date
        .as_deref()
        .and_then(parse_date)
        .unwrap_or(today)
        .format("%Y-%m-%d")
        .to_string();

    let chains = ["tsn", "csn"].map(|metric| build_chain(metric, &observations, &event_rank));
    let [tsn_chain, csn_chain] = &chains;

    let mut out = Findings::default();
    let life = compute_life(pkg, &observations, tsn_chain, csn_chain, cutoff.as_deref(), &mut out);
    let (timeline, stints) = build_timeline(&events, tsn_chain, csn_chain, &mut out);
    check_chains(tsn_chain, csn_chain, &observations, &mut out);
    check_domain_constraints(pkg, &observations, tsn_chain, csn_chain, &stints, &mut out);
    check_documents(
        pkg,
        &events,
        &observations,
        csn_chain,
        cutoff.as_deref(),
        &review_date,
        &docs,
        &mut out,
    );
    let evidence = build_graph(&chains, &observations);

    for item in &pkg.open_items {
        out.item(
            &item.kind,
            parse_severity(&item.severity),
            &item.title,
            &item.detail,
            &item.request_from,
            item.documents.clone(),
            item.observations.clone(),
            "declared",
        );
    }
    out.items
        .sort_by(|a, b| a.severity.cmp(&b.severity).then(a.kind.cmp(&b.kind)));
    for (i, item) in out.items.iter_mut().enumerate() {
        item.id = format!("OI-{:02}", i + 1);
    }

    let verdict = if out.checks.iter().any(|c| c.result == CheckResult::Fail) {
        Verdict::Fail
    } else if !out.items.is_empty() || out.checks.iter().any(|c| c.result == CheckResult::Warn) {
        Verdict::Conditional
    } else {
        Verdict::Pass
    };

    ComponentReview {
        schema: RESULT_SCHEMA.into(),
        component: pkg.component.clone(),
        cutoff_date: cutoff,
        review_date,
        verdict,
        life,
        timeline,
        stints,
        open_items: out.items,
        checks: out.checks,
        evidence,
        part_tree: knowledge::build_tree(&pkg.component, &pkg.hierarchy, &pkg.siblings),
        documents: pkg.documents.clone(),
    }
}

fn date_key(raw: &str) -> (u8, String) {
    match parse_date(raw) {
        Some(d) => (0, d.format("%Y-%m-%d").to_string()),
        None => (1, raw.to_string()),
    }
}

fn parse_severity(raw: &str) -> Severity {
    match raw {
        "high" | "P1" => Severity::High,
        "low" | "P3" => Severity::Low,
        _ => Severity::Medium,
    }
}

fn display(metric: &str, value: f64) -> String {
    if metric == "tsn" {
        format_hours(value)
    } else {
        format_cycles(value)
    }
}

#[derive(Default)]
struct Findings {
    items: Vec<OpenItem>,
    checks: Vec<Check>,
}

impl Findings {
    #[allow(clippy::too_many_arguments)]
    fn item(
        &mut self,
        kind: &str,
        severity: Severity,
        title: &str,
        detail: &str,
        request_from: &str,
        documents: Vec<String>,
        observations: Vec<String>,
        rule: &str,
    ) {
        self.items.push(OpenItem {
            id: String::new(),
            kind: kind.into(),
            severity,
            title: title.into(),
            detail: detail.into(),
            request_from: request_from.into(),
            documents,
            observations,
            rule: rule.into(),
        });
    }

    fn check(&mut self, rule: &str, result: CheckResult, detail: String) {
        self.checks.push(Check {
            rule: rule.into(),
            result,
            detail,
        });
    }
}

// ---------------------------------------------------------------------------
// Chains
// ---------------------------------------------------------------------------

fn build_chain(metric: &str, observations: &[Obs], event_rank: &HashMap<&str, usize>) -> EvidenceChain {
    let mut groups: BTreeMap<(String, usize, String), Vec<&Obs>> = BTreeMap::new();
    for o in observations
        .iter()
        .filter(|o| o.raw.metric == metric && o.value.is_some())
    {
        let event = o.raw.event.clone().unwrap_or_default();
        let rank = event_rank.get(event.as_str()).copied().unwrap_or(usize::MAX);
        let key = if event.is_empty() { o.date.clone() } else { event };
        groups.entry((date_key(&o.date).1, rank, key)).or_default().push(o);
    }
    let points = groups
        .into_iter()
        .enumerate()
        .map(|(i, ((date, _, _), mut members))| {
            members.sort_by_key(|o| std::cmp::Reverse((o.is_human(), o.precision())));
            let value = members[0].value.unwrap_or_default();
            let spread = members
                .iter()
                .filter_map(|o| o.value)
                .map(|v| (v - value).abs())
                .fold(0.0_f64, f64::max);
            let has_human = members.iter().any(|o| o.is_human());
            let consensus = if members.len() == 1 || has_human {
                if members.len() == 1 {
                    Consensus::Single
                } else if spread == 0.0 || (metric == "tsn" && spread < 0.01) {
                    Consensus::Consistent
                } else if metric == "tsn" && spread <= TSN_ROUNDING_HOURS {
                    Consensus::Rounding
                } else {
                    Consensus::Consistent
                }
            } else if spread == 0.0 || (metric == "tsn" && spread < 0.01) {
                Consensus::Consistent
            } else if metric == "tsn" && spread <= TSN_ROUNDING_HOURS {
                Consensus::Rounding
            } else {
                Consensus::Conflict
            };
            ChainPoint {
                id: format!("{metric}-p{}", i + 1),
                date,
                event: members[0].raw.event.clone(),
                value,
                display: display(metric, value),
                consensus,
                observations: members.iter().map(|o| o.raw.id.clone()).collect(),
            }
        })
        .collect();
    EvidenceChain {
        metric: metric.into(),
        points,
    }
}

fn at_event(chain: &EvidenceChain, event: &str) -> Option<f64> {
    chain
        .points
        .iter()
        .find(|p| p.event.as_deref() == Some(event))
        .map(|p| p.value)
}

fn check_chains(tsn: &EvidenceChain, csn: &EvidenceChain, observations: &[Obs], out: &mut Findings) {
    for chain in [tsn, csn] {
        let label = chain.metric.to_uppercase();
        let tolerance = if chain.metric == "tsn" { TSN_ROUNDING_HOURS } else { 1.0 };
        let regressions: Vec<_> = chain
            .points
            .windows(2)
            .filter(|w| w[1].value + tolerance < w[0].value)
            .collect();
        for w in &regressions {
            out.item(
                "suspicious",
                Severity::High,
                &format!("{label} 回退：{} → {}", w[0].date, w[1].date),
                &format!("{label} 从 {} 降到 {}，时间链不可单调累加", w[0].display, w[1].display),
                "运营方 / MRO",
                Vec::new(),
                w[1].observations.clone(),
                &format!("{}_regression", chain.metric),
            );
        }
        if chain.points.is_empty() {
            out.check(
                &format!("{label} 单调性"),
                CheckResult::Unknown,
                format!("未提取到 {label} 读数"),
            );
        } else if regressions.is_empty() {
            out.check(
                &format!("{label} 单调性"),
                CheckResult::Pass,
                format!("{} 个节点按时间非递减", chain.points.len()),
            );
        } else {
            out.check(
                &format!("{label} 单调性"),
                CheckResult::Fail,
                format!("{} 处回退", regressions.len()),
            );
        }

        let conflicts: Vec<_> = chain
            .points
            .iter()
            .filter(|p| p.consensus == Consensus::Conflict)
            .collect();
        for p in &conflicts {
            let readings: Vec<String> = p
                .observations
                .iter()
                .filter_map(|id| observations.iter().find(|o| &o.raw.id == id))
                .map(|o| format!("{}（{} p.{}）", o.raw.raw, o.raw.doc, o.raw.page))
                .collect();
            out.item(
                "suspicious",
                Severity::High,
                &format!("{label} 多源不一致 @ {}", p.date),
                &format!("同一节点读数不一致：{}", readings.join("；")),
                "运营方 / MRO",
                Vec::new(),
                p.observations.clone(),
                "cross_source_conflict",
            );
        }
        let rounding = chain
            .points
            .iter()
            .filter(|p| p.consensus == Consensus::Rounding)
            .count();
        let multi = chain.points.iter().filter(|p| p.consensus != Consensus::Single).count();
        let result = if !conflicts.is_empty() {
            CheckResult::Fail
        } else if multi == 0 {
            CheckResult::Warn
        } else {
            CheckResult::Pass
        };
        out.check(
            &format!("{label} 交叉验证"),
            result,
            format!(
                "{multi} 个节点有多源印证，{rounding} 个为单位换算误差，{} 个冲突",
                conflicts.len()
            ),
        );
    }
}

fn check_domain_constraints(
    pkg: &ReviewPackage,
    observations: &[Obs],
    tsn: &EvidenceChain,
    csn: &EvidenceChain,
    stints: &[Stint],
    out: &mut Findings,
) {
    if let (Some(limit), Some(current)) = (
        observations
            .iter()
            .find(|o| o.raw.metric == "life_limit_cycles" && o.value.is_some())
            .and_then(|o| o.value),
        csn.points.last().and_then(|p| (p.consensus != Consensus::Conflict).then_some(p.value)),
    ) && current > limit + 0.5
    {
        out.item(
            "suspicious",
            Severity::High,
            "CSN 超出寿命上限",
            &format!("当前 CSN {} 大于寿命限制 {}", format_cycles(current), format_cycles(limit)),
            "运营方 / OEM",
            Vec::new(),
            Vec::new(),
            "csn_over_limit",
        );
    }

    if let (Some(hours), Some(cycles)) = (tsn.points.last().map(|p| p.value), csn.points.last().map(|p| p.value))
        && cycles > 0.0
    {
        let ratio = hours / cycles;
        if !(0.5..=50.0).contains(&ratio) {
            out.item(
                "suspicious",
                Severity::High,
                "小时/循环比异常",
                &format!("TSN/CSN = {ratio:.2}，通常落在 0.5–50。读错一位数几乎都会撞破这条"),
                "运营方 / MRO",
                Vec::new(),
                Vec::new(),
                "hours_cycles_ratio",
            );
            out.check("小时循环比", CheckResult::Fail, format!("比值为 {ratio:.2}"));
        } else {
            out.check("小时循环比", CheckResult::Pass, format!("比值为 {ratio:.2}"));
        }
    }

    for stint in stints {
        if let (Some(used), Some(end), Some(start)) = (
            stint.used_csn,
            csn.points.iter().find(|p| p.event.as_deref() == Some(stint.removal_event.as_str())).map(|p| p.value),
            csn.points.iter().find(|p| p.event.as_deref() == Some(stint.install_event.as_str())).map(|p| p.value),
        ) && (used - (end - start)).abs() > 0.5
        {
            out.item(
                "suspicious",
                Severity::High,
                &format!("在翼段用量不自洽 {} → {}", stint.from, stint.to),
                &format!("used_csn={used} 但 end−start={}", end - start),
                "运营方 / MRO",
                Vec::new(),
                Vec::new(),
                "stint_arithmetic",
            );
        }
    }

    for obs in observations {
        if let Some(date) = parse_date(&obs.date)
            && !(1970..=2035).contains(&date.year())
        {
            out.item(
                "suspicious",
                Severity::Medium,
                &format!("日期超出合理年份 {}", obs.date),
                "日期应落在 1970–2035",
                "运营方 / MRO",
                Vec::new(),
                vec![obs.raw.id.clone()],
                "date_range",
            );
        }
    }

    let mut pn: Vec<&str> = observations
        .iter()
        .filter(|o| o.raw.metric == "part_number")
        .map(|o| o.raw.raw.as_str())
        .collect();
    pn.sort_unstable();
    pn.dedup();
    if pn.len() > 1 {
        out.item(
            "suspicious",
            Severity::High,
            "件号跨文档不一致",
            &format!("读到多个件号：{}", pn.join(" / ")),
            "运营方 / MRO",
            Vec::new(),
            Vec::new(),
            "part_number_mismatch",
        );
    }
    if !pkg.component.part_number.is_empty()
        && pn.iter().any(|value| normalize_ident(value) != normalize_ident(&pkg.component.part_number))
    {
        out.item(
            "suspicious",
            Severity::High,
            "件号与项目身份不一致",
            "文件里的件号和当前审查件号对不上",
            "运营方 / MRO",
            Vec::new(),
            Vec::new(),
            "part_number_identity",
        );
    }
}

fn normalize_ident(value: &str) -> String {
    value.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_ascii_uppercase()
}

// ---------------------------------------------------------------------------
// Life
// ---------------------------------------------------------------------------

fn latest_until<'a>(chain: &'a EvidenceChain, cutoff: Option<&str>) -> Option<&'a ChainPoint> {
    chain
        .points
        .iter()
        .rev()
        .find(|p| {
            p.consensus != Consensus::Conflict
                && cutoff.is_none_or(|c| p.date.as_str() <= c || parse_date(&p.date).is_none())
        })
}

fn documented_limit(metric: &str, observations: &[Obs]) -> Option<LimitValue> {
    let hits: Vec<&Obs> = observations
        .iter()
        .filter(|o| o.raw.metric == metric && o.value.is_some())
        .collect();
    let mut tally: BTreeMap<i64, Vec<&Obs>> = BTreeMap::new();
    for o in &hits {
        tally
            .entry(o.value.unwrap_or_default().round() as i64)
            .or_default()
            .push(o);
    }
    let (value, members) = tally.into_iter().max_by_key(|(_, m)| m.len())?;
    let pages: Vec<String> = members
        .iter()
        .map(|o| format!("{} p.{}", o.raw.doc, o.raw.page))
        .collect();
    Some(LimitValue {
        value: value as f64,
        source: "documents".into(),
        detail: format!("文件记载：{}", pages.join("、")),
        observations: members.iter().map(|o| o.raw.id.clone()).collect(),
    })
}

fn compute_life(
    pkg: &ReviewPackage,
    observations: &[Obs],
    tsn: &EvidenceChain,
    csn: &EvidenceChain,
    cutoff: Option<&str>,
    out: &mut Findings,
) -> LifeSummary {
    let metric = |chain: &EvidenceChain| {
        latest_until(chain, cutoff).map(|p| MetricValue {
            value: p.value,
            display: p.display.clone(),
            as_of: p.date.clone(),
            observations: p.observations.clone(),
        })
    };
    let mut life = LifeSummary {
        tsn: metric(tsn),
        csn: metric(csn),
        ..Default::default()
    };

    let kb_limit = knowledge::lookup(&pkg.component).and_then(|hit| {
        let rating = if pkg.component.thrust_rating.is_empty() {
            rating_from_model(&pkg.component.asset_model)
        } else {
            pkg.component.thrust_rating.clone()
        };
        let part = hit.part?;
        part.limit_for(&rating).map(|(value, note)| LimitValue {
            value,
            source: "knowledge_base".into(),
            detail: format!("{} 知识库 {} {}", hit.asset.asset_model, part.part_number, note)
                .trim()
                .to_string(),
            observations: Vec::new(),
        })
    });
    let doc_limit = documented_limit("life_limit_cycles", observations);
    if let (Some(d), Some(k)) = (&doc_limit, &kb_limit)
        && (d.value - k.value).abs() > 0.5
    {
        out.item(
            "suspicious",
            Severity::High,
            "寿命限制与知识库不一致",
            &format!(
                "文件记载 {}，知识库为 {}（{}）",
                format_cycles(d.value),
                format_cycles(k.value),
                k.detail
            ),
            "OEM / 运营方工程",
            Vec::new(),
            d.observations.clone(),
            "life_limit_mismatch",
        );
    }
    life.life_limit_cycles = doc_limit.or(kb_limit);
    life.life_limit_hours = documented_limit("life_limit_hours", observations);

    if let (Some(limit), Some(csn)) = (&life.life_limit_cycles, &life.csn) {
        let remaining = limit.value - csn.value;
        life.remaining_cycles = Some(remaining);
        life.used_ratio = Some(csn.value / limit.value);
        life.formulas.push(format!(
            "剩余循环 = {} − {} = {}",
            format_cycles(limit.value),
            format_cycles(csn.value),
            format_cycles(remaining)
        ));
        if remaining < 0.0 {
            out.check(
                "寿命限制",
                CheckResult::Fail,
                format!("CSN {} 已超出限制 {}", csn.display, format_cycles(limit.value)),
            );
        } else {
            out.check(
                "寿命限制",
                CheckResult::Pass,
                format!("剩余 {} 循环（{}）", format_cycles(remaining), limit.detail),
            );
        }
    } else if life.life_limit_cycles.is_none() {
        out.item(
            "missing",
            Severity::High,
            "缺少寿命限制来源",
            "文件中未找到循环寿命限制，知识库也无此件号，无法计算剩余寿命",
            "OEM / 运营方工程",
            Vec::new(),
            Vec::new(),
            "missing_life_limit",
        );
        out.check("寿命限制", CheckResult::Fail, "无寿命限制来源".into());
    } else {
        out.check("寿命限制", CheckResult::Unknown, "缺少当前 CSN，无法计算剩余".into());
    }
    if let (Some(limit), Some(tsn)) = (&life.life_limit_hours, &life.tsn) {
        let remaining = limit.value - tsn.value;
        life.remaining_hours = Some(remaining);
        life.formulas.push(format!(
            "剩余小时 = {} − {} = {}",
            format_hours(limit.value),
            tsn.display,
            format_hours(remaining)
        ));
    }
    life
}

// ---------------------------------------------------------------------------
// Timeline
// ---------------------------------------------------------------------------

fn build_timeline(
    events: &[&EventEntry],
    tsn: &EvidenceChain,
    csn: &EvidenceChain,
    out: &mut Findings,
) -> (Vec<TimelineEntry>, Vec<Stint>) {
    let mut entries: Vec<TimelineEntry> = events
        .iter()
        .map(|e| {
            let observations = tsn
                .points
                .iter()
                .chain(csn.points.iter())
                .filter(|p| p.event.as_deref() == Some(e.id.as_str()))
                .flat_map(|p| p.observations.clone())
                .collect();
            TimelineEntry {
                id: e.id.clone(),
                date: normalize_date(&e.date),
                kind: e.kind.clone(),
                label: e.label.clone(),
                host: e.host.clone(),
                operator: e.operator.clone(),
                mro: e.mro.clone(),
                tsn: at_event(tsn, &e.id),
                csn: at_event(csn, &e.id),
                documents: e.documents.clone(),
                observations,
                flags: Vec::new(),
            }
        })
        .collect();

    let mut stints = Vec::new();
    let mut open_install: Option<usize> = None;
    let mut last_removal: Option<usize> = None;
    for i in 0..entries.len() {
        match entries[i].kind.as_str() {
            "install" => {
                if let Some(r) = last_removal {
                    let gap_c = entries[i].csn.zip(entries[r].csn).map(|(a, b)| a - b);
                    let gap_t = entries[i].tsn.zip(entries[r].tsn).map(|(a, b)| a - b);
                    if gap_c.is_some_and(|d| d.abs() > 0.5) || gap_t.is_some_and(|d| d.abs() > TSN_ROUNDING_HOURS) {
                        entries[i].flags.push("discontinuity".into());
                        out.item(
                            "suspicious",
                            Severity::High,
                            &format!("装机读数未延续上次拆下 @ {}", entries[i].date),
                            &format!(
                                "{} 拆下时 {} / {}，{} 装机时 {} / {}",
                                entries[r].date,
                                entries[r].tsn.map(format_hours).unwrap_or("—".into()),
                                entries[r].csn.map(format_cycles).unwrap_or("—".into()),
                                entries[i].date,
                                entries[i].tsn.map(format_hours).unwrap_or("—".into()),
                                entries[i].csn.map(format_cycles).unwrap_or("—".into()),
                            ),
                            "运营方 / MRO",
                            entries[i].documents.clone(),
                            entries[i].observations.clone(),
                            "install_discontinuity",
                        );
                    }
                }
                open_install = Some(i);
            }
            "removal" | "current" => {
                if let Some(s) = open_install {
                    let used_tsn = entries[i].tsn.zip(entries[s].tsn).map(|(a, b)| a - b);
                    let used_csn = entries[i].csn.zip(entries[s].csn).map(|(a, b)| a - b);
                    if used_csn.is_some_and(|d| d < 0.0) || used_tsn.is_some_and(|d| d < -TSN_ROUNDING_HOURS) {
                        entries[i].flags.push("negative_usage".into());
                        out.item(
                            "suspicious",
                            Severity::High,
                            &format!("装拆区间用量为负 {} → {}", entries[s].date, entries[i].date),
                            "拆下读数小于装机读数",
                            "运营方 / MRO",
                            entries[i].documents.clone(),
                            entries[i].observations.clone(),
                            "invalid_stint_usage",
                        );
                    }
                    stints.push(Stint {
                        install_event: entries[s].id.clone(),
                        removal_event: entries[i].id.clone(),
                        from: entries[s].date.clone(),
                        to: entries[i].date.clone(),
                        host: entries[s].host.clone(),
                        operator: entries[s].operator.clone(),
                        used_tsn,
                        used_csn,
                    });
                }
                if entries[i].kind == "removal" {
                    open_install = None;
                    last_removal = Some(i);
                }
            }
            _ => {}
        }
    }

    let negative = stints.iter().filter(|s| s.used_csn.is_some_and(|d| d < 0.0)).count();
    out.check(
        "装拆配对",
        if stints.is_empty() {
            CheckResult::Unknown
        } else if negative > 0 {
            CheckResult::Fail
        } else {
            CheckResult::Pass
        },
        format!("{} 个在翼区间，{} 个用量为负", stints.len(), negative),
    );

    for w in 0..entries.len().saturating_sub(1) {
        let (Some(a), Some(b)) = (parse_date(&entries[w].date), parse_date(&entries[w + 1].date)) else {
            continue;
        };
        if (b - a).num_days() > GAP_YEARS * 365 {
            entries[w + 1].flags.push("gap".into());
            out.item(
                "missing",
                Severity::Medium,
                &format!("履历断档 {} → {}", entries[w].date, entries[w + 1].date),
                &format!(
                    "相邻记录间隔 {} 天，超过 {GAP_YEARS} 年，需补充该期间装拆或在翼记录",
                    (b - a).num_days()
                ),
                "运营方 / CAMO",
                Vec::new(),
                Vec::new(),
                "temporal_gap",
            );
        }
    }
    (entries, stints)
}

// ---------------------------------------------------------------------------
// Documents
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn check_documents(
    pkg: &ReviewPackage,
    events: &[&EventEntry],
    observations: &[Obs],
    csn: &EvidenceChain,
    cutoff: Option<&str>,
    review_date: &str,
    docs: &HashMap<&str, &DocumentEntry>,
    out: &mut Findings,
) {
    let has_birth = events.iter().any(|e| e.kind == "birth");
    let zero_baseline = csn.points.first().is_some_and(|p| p.value == 0.0);
    if has_birth {
        out.check("出生记录", CheckResult::Pass, "已识别出生/出厂节点".into());
    } else if zero_baseline {
        out.item(
            "missing",
            Severity::Medium,
            "缺少部件出生文件",
            "首次装机为 0/0 基线，时间链可起算，但缺少能以该件号/序号证明零寿命起点的出厂文件",
            "OEM / 卖方",
            Vec::new(),
            csn.points[0].observations.clone(),
            "missing_birth_doc_only",
        );
        out.check(
            "出生记录",
            CheckResult::Warn,
            "无出生文件，但首条记录为 0/0 基线".into(),
        );
    } else {
        out.item(
            "missing",
            Severity::High,
            "缺少出生记录且首条读数非零",
            "无法证明该件初始合规，首条记录前的使用量没有来源",
            "OEM / 卖方",
            Vec::new(),
            Vec::new(),
            "missing_birth",
        );
        out.check("出生记录", CheckResult::Fail, "无出生节点且无零基线".into());
    }

    for e in events.iter().filter(|e| e.kind == "shop_visit") {
        let released = e
            .documents
            .iter()
            .filter_map(|id| docs.get(id.as_str()))
            .any(|d| d.doc_type == "release_certificate");
        if !released {
            out.item(
                "missing",
                Severity::High,
                &format!("车间送修缺少放行证书 @ {}", normalize_date(&e.date)),
                &format!(
                    "{} 的送修/车间事件未关联 FAA 8130-3 / EASA Form 1",
                    if e.mro.is_empty() { "该次" } else { &e.mro }
                ),
                if e.mro.is_empty() { "MRO" } else { &e.mro },
                e.documents.clone(),
                Vec::new(),
                "shop_visit_without_release",
            );
        }
    }

    if let Some(last) = latest_until(csn, cutoff) {
        let supporting: Vec<&DocumentEntry> = last
            .observations
            .iter()
            .filter_map(|id| observations.iter().find(|o| &o.raw.id == id))
            .filter_map(|o| o.doc)
            .collect();
        if !supporting.is_empty() && supporting.iter().all(|d| d.signed != Some(true)) {
            out.item(
                "suspicious",
                Severity::Medium,
                &format!("当前状态 {} 仅有未签署文件支撑", last.date),
                &format!("CSN {} 只来自系统打印件，无签署的状态表或车间文件", last.display),
                "运营方 / CAMO",
                supporting.iter().map(|d| d.id.clone()).collect(),
                last.observations.clone(),
                "unsigned_current_status",
            );
        }
        let end = cutoff.unwrap_or(last.date.as_str());
        if let (Some(a), Some(b)) = (parse_date(end), parse_date(review_date))
            && (b - a).num_days() > STALE_CUTOFF_DAYS
        {
            out.item(
                "missing",
                Severity::High,
                &format!("{end} 之后无任何记录"),
                &format!(
                    "截止日到审查日 {review_date} 共 {} 天无状态表或拆装记录",
                    (b - a).num_days()
                ),
                "运营方 / CAMO",
                Vec::new(),
                Vec::new(),
                "stale_cutoff",
            );
            out.check(
                "截止日对齐",
                CheckResult::Warn,
                format!("最新记录距审查日 {} 天", (b - a).num_days()),
            );
        } else {
            out.check("截止日对齐", CheckResult::Pass, format!("最新记录 {}", last.date));
        }
    }

    for doc in &pkg.documents {
        if !matches!(doc.parse_quality.as_str(), "failed" | "weak") {
            continue;
        }
        let used: Vec<String> = observations
            .iter()
            .filter(|o| o.raw.doc == doc.id)
            .map(|o| o.raw.id.clone())
            .collect();
        if used.is_empty() {
            continue;
        }
        out.item(
            "suspicious",
            Severity::Medium,
            &format!(
                "{} 读数需人工复核",
                if doc.title.is_empty() { &doc.id } else { &doc.title }
            ),
            &format!("该文件解析质量为 {}，其读数参与了计算", doc.parse_quality),
            if doc.party.is_empty() {
                "运营方 / MRO"
            } else {
                &doc.party
            },
            vec![doc.id.clone()],
            used,
            "weak_parse",
        );
    }
}

// ---------------------------------------------------------------------------
// Evidence graph
// ---------------------------------------------------------------------------

/// (document, page, date) — TSN and CSN read together from one page row.
type PageKey = (String, u32, String);

fn build_graph(chains: &[EvidenceChain; 2], observations: &[Obs]) -> EvidenceGraph {
    let point_of: HashMap<&str, &str> = chains
        .iter()
        .flat_map(|c| {
            c.points
                .iter()
                .flat_map(|p| p.observations.iter().map(move |o| (o.as_str(), p.id.as_str())))
        })
        .collect();
    let cards: Vec<EvidenceCard> = observations
        .iter()
        .filter(|o| point_of.contains_key(o.raw.id.as_str()) || o.raw.metric.starts_with("life_limit"))
        .map(|o| EvidenceCard {
            id: o.raw.id.clone(),
            metric: o.raw.metric.clone(),
            point: point_of.get(o.raw.id.as_str()).map(|s| s.to_string()),
            doc: o.raw.doc.clone(),
            doc_title: o.doc.map(|d| d.title.clone()).unwrap_or_default(),
            doc_type: o.doc.map(|d| d.doc_type.clone()).unwrap_or_default(),
            file: o.doc.map(|d| d.file.clone()).unwrap_or_default(),
            page: o.raw.page,
            bbox: o.raw.bbox,
            raw: o.raw.raw.clone(),
            value: o.value,
            date: o.date.clone(),
            signed: o.doc.and_then(|d| d.signed),
            parse_quality: o.doc.map(|d| d.parse_quality.clone()).unwrap_or_default(),
        })
        .collect();

    let value_of: HashMap<&str, f64> = observations
        .iter()
        .filter_map(|o| o.value.map(|v| (o.raw.id.as_str(), v)))
        .collect();
    let mut links = Vec::new();
    let mut push = |source: &str, target: &str, kind: LinkKind, label: String| {
        links.push(EvidenceLink {
            id: format!("l{}", links.len() + 1),
            source: source.into(),
            target: target.into(),
            kind,
            label,
        });
    };
    for chain in chains {
        for w in chain.points.windows(2) {
            let delta = w[1].value - w[0].value;
            let label = if chain.metric == "tsn" {
                format!("+{}", format_hours(delta))
            } else {
                format!("+{}", format_cycles(delta))
            };
            push(&w[0].observations[0], &w[1].observations[0], LinkKind::Sequence, label);
        }
        for p in &chain.points {
            let primary = &p.observations[0];
            let base = value_of.get(primary.as_str()).copied().unwrap_or_default();
            for other in &p.observations[1..] {
                let diff = (value_of.get(other.as_str()).copied().unwrap_or_default() - base).abs();
                let tolerance = if chain.metric == "tsn" { TSN_ROUNDING_HOURS } else { 0.0 };
                if diff > tolerance {
                    push(
                        primary,
                        other,
                        LinkKind::Conflicts,
                        format!("Δ{}", display(&chain.metric, diff)),
                    );
                } else {
                    push(
                        primary,
                        other,
                        LinkKind::Corroborates,
                        if diff > 0.0 {
                            "换算误差".into()
                        } else {
                            "一致".into()
                        },
                    );
                }
            }
        }
    }
    let mut seen: HashMap<PageKey, (Option<&str>, Option<&str>)> = HashMap::new();
    for o in observations.iter().filter(|o| point_of.contains_key(o.raw.id.as_str())) {
        let key = (o.raw.doc.clone(), o.raw.page, o.date.clone());
        let slot = seen.entry(key).or_default();
        match o.raw.metric.as_str() {
            "tsn" if slot.0.is_none() => slot.0 = Some(o.raw.id.as_str()),
            "csn" if slot.1.is_none() => slot.1 = Some(o.raw.id.as_str()),
            _ => {}
        }
    }
    let mut pairs: Vec<(&str, &str)> = seen.values().filter_map(|(t, c)| t.zip(*c)).collect();
    pairs.sort();
    for (t, c) in pairs {
        push(t, c, LinkKind::SamePage, "同页".into());
    }

    EvidenceGraph {
        chains: chains.to_vec(),
        cards,
        links,
    }
}
