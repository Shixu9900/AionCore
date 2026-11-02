use aionui_records_review::*;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::NaiveDate;
use http_body_util::BodyExt;
use tower::ServiceExt;

fn fixture() -> ReviewPackage {
    serde_json::from_str(include_str!("fixtures/fan_blade_rgf34904.json")).unwrap()
}

fn today() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 10, 28).unwrap()
}

#[test]
fn fan_blade_life_matches_manual_audit() {
    let r = review(&fixture(), today());
    let csn = r.life.csn.as_ref().unwrap();
    let tsn = r.life.tsn.as_ref().unwrap();
    assert_eq!(csn.value, 4585.0);
    assert_eq!(csn.as_of, "2023-07-13");
    assert_eq!(tsn.display, "29,189:16");
    let limit = r.life.life_limit_cycles.as_ref().unwrap();
    assert_eq!(limit.value, 14336.0);
    assert_eq!(limit.source, "documents");
    assert_eq!(r.life.remaining_cycles, Some(9751.0));
    assert_eq!(r.verdict, Verdict::Conditional);
}

#[test]
fn fan_blade_stints_match_installed_periods() {
    let r = review(&fixture(), today());
    let used: Vec<f64> = r.stints.iter().filter_map(|s| s.used_csn).collect();
    assert_eq!(used, vec![679.0, 1476.0, 2430.0]);
    assert!(
        r.timeline
            .iter()
            .all(|e| !e.flags.contains(&"discontinuity".to_string()))
    );
}

#[test]
fn fan_blade_chains_cross_validate_with_rounding_only() {
    let r = review(&fixture(), today());
    let tsn = r.evidence.chains.iter().find(|c| c.metric == "tsn").unwrap();
    assert!(tsn.points.iter().all(|p| p.consensus != Consensus::Conflict));
    assert!(tsn.points.iter().any(|p| p.consensus == Consensus::Rounding));
    assert!(r.evidence.links.iter().any(|l| l.kind == LinkKind::SamePage));
    assert!(r.evidence.cards.iter().all(|c| c.bbox.is_some() && !c.file.is_empty()));
}

#[test]
fn fan_blade_open_items_cover_gaps() {
    let r = review(&fixture(), today());
    let rules: Vec<&str> = r.open_items.iter().map(|i| i.rule.as_str()).collect();
    assert!(rules.contains(&"stale_cutoff"));
    assert!(rules.contains(&"missing_birth_doc_only"));
    assert!(rules.contains(&"unsigned_current_status"));
    assert!(rules.contains(&"declared"));
    assert!(r.open_items.iter().any(|i| i.kind == "suspicious"));
    assert_eq!(r.open_items[0].id, "OI-01");
}

#[test]
fn fan_blade_tree_comes_from_documents() {
    let r = review(&fixture(), today());
    assert_eq!(r.part_tree.source, "documents");
    let root = r.part_tree.root.as_ref().unwrap();
    assert_eq!(root.level, 1);
    let module = &root.children[0];
    assert_eq!(module.level, 2);
    assert_eq!(module.children_count, 26);
    assert!(module.children.iter().any(|c| c.focus && c.serial_number == "RGF34904"));
}

#[test]
fn conflicting_key_fields_do_not_choose_a_winner() {
    let mut pkg = fixture();
    let mut extra = pkg
        .observations
        .iter()
        .find(|o| o.metric == "csn")
        .cloned()
        .expect("csn reading");
    extra.id = "conflict-csn".into();
    extra.raw = "99999".into();
    extra.value = Some(99999.0);
    extra.source = "engine".into();
    pkg.observations.push(extra);
    let r = review(&pkg, today());
    assert!(
        r.checks
            .iter()
            .any(|c| c.rule.contains("交叉验证") && c.result == CheckResult::Fail)
    );
    assert!(r.open_items.iter().any(|i| i.rule == "cross_source_conflict"));
}

#[test]
fn regression_and_conflict_fail_the_review() {
    let mut pkg = fixture();
    let removal = pkg
        .observations
        .iter_mut()
        .find(|o| o.metric == "csn" && o.event.as_deref() == Some("E6"))
        .unwrap();
    removal.value = Some(500.0);
    let r = review(&pkg, today());
    assert_eq!(r.verdict, Verdict::Fail);
    assert!(r.open_items.iter().any(|i| i.rule == "cross_source_conflict"));
}

#[test]
fn lookup_card_selects_the_focused_serial_and_life_focus() {
    let card = lookup_card(&review(&fixture(), today()), "RGF34904 还剩多少寿命");
    assert!(card.found);
    assert_eq!(card.serial_number, "RGF34904");
    assert_eq!(card.page, "llp");
    assert_eq!(card.focus, "life");
    assert_eq!(card.part, "RGF34904");
    assert_eq!(card.remaining_cycles, Some(9751.0));
    assert_eq!(card.verdict.as_deref(), Some("conditional"));
    assert!(card.open_items > 0);
}

#[tokio::test]
async fn lookup_route_returns_the_card() {
    let body = serde_json::json!({
        "query": "RGF34904",
        "package": serde_json::from_str::<serde_json::Value>(include_str!("fixtures/fan_blade_rgf34904.json")).unwrap(),
    });
    let res = records_review_routes()
        .oneshot(
            Request::post("/api/records-review/lookup")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["data"]["serial_number"], "RGF34904");
    assert_eq!(json["data"]["page"], "llp");
}

#[tokio::test]
async fn compute_route_returns_review() {
    let body = include_str!("fixtures/fan_blade_rgf34904.json");
    let res = records_review_routes()
        .oneshot(
            Request::post("/api/records-review/compute")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["data"]["schema"], RESULT_SCHEMA);
}

#[tokio::test]
async fn compute_route_rejects_unknown_schema() {
    let res = records_review_routes()
        .oneshot(
            Request::post("/api/records-review/compute")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"schema":"x","component":{"part_number":"A","serial_number":"B"}}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}
