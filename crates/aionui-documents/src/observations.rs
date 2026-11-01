use chrono::Utc;
use uuid::Uuid;

use aionui_records_review::{DocumentEntry, Observation, ReviewPackage};

use crate::store::DocumentStore;
use crate::types::{AnnotateRequest, ObservationRow};

pub async fn annotate(store: &DocumentStore, req: AnnotateRequest) -> Result<ObservationRow, sqlx::Error> {
    let existing = store.list_observations(&req.project_id, Some(&req.sha256)).await?;
    for old in existing.iter().filter(|o| o.metric == req.metric && o.page_no == req.page_no) {
        let mut stale = old.clone();
        stale.status = "stale".into();
        stale.superseded_by = Some(format!("pending-{}", req.metric));
        store.upsert_observation(&stale).await?;
    }
    let row = ObservationRow {
        obs_id: Uuid::now_v7().to_string(),
        project_id: req.project_id,
        sha256: req.sha256,
        page_no: req.page_no,
        bbox: req.bbox,
        metric: req.metric,
        raw: req.raw,
        value: req.value,
        confidence: Some(1.0),
        source: "human".into(),
        status: "accepted".into(),
        block_id: None,
        superseded_by: None,
        confirmed_at: Some(Utc::now().to_rfc3339()),
    };
    store.upsert_observation(&row).await?;
    Ok(row)
}

pub async fn package_from_store(
    store: &DocumentStore,
    project_id: &str,
    mut pkg: ReviewPackage,
) -> Result<ReviewPackage, sqlx::Error> {
    let docs = store.list_project(project_id).await?;
    if pkg.documents.is_empty() {
        pkg.documents = docs
            .iter()
            .map(|d| DocumentEntry {
                id: d.doc_id.clone(),
                title: d.filename.clone(),
                file: d.path.clone(),
                pages: (1..=d.page_count).collect(),
                parse_quality: "good".into(),
                ..Default::default()
            })
            .collect();
    }
    let stored = store.list_observations(project_id, None).await?;
    for row in stored {
        if row.status == "withdrawn" || row.status == "stale" {
            continue;
        }
        if row.source != "human" && row.status != "accepted" {
            continue;
        }
        let doc_id = docs
            .iter()
            .find(|d| d.sha256 == row.sha256)
            .map(|d| d.doc_id.clone())
            .unwrap_or_else(|| row.sha256.clone());
        pkg.observations.push(Observation {
            id: row.obs_id,
            doc: doc_id,
            page: row.page_no,
            bbox: row.bbox,
            metric: row.metric,
            raw: row.raw,
            value: row.value,
            event: None,
            as_of: None,
            source: row.source,
        });
    }
    Ok(pkg)
}
