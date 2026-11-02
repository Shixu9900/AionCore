#![allow(clippy::disallowed_types)]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use aionui_api_types::ApiResponse;
use aionui_common::ApiError;
use aionui_records_review::{ReviewPackage, review};
use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::cascade::ParseOptions;
use crate::golden::{GoldenField, seed_41744};
use crate::ingest::{FsChange, IngestLimits, add_file, apply_fs_event, scan_folder};
use crate::lifecycle::MinerULifecycle;
use crate::observations::{annotate, package_from_store};
use crate::provider::{CloudParseProvider, LocalDoclibProvider};
use crate::queue::{ParseQueue, QueueProgress};
use crate::retrieval::search_project;
use crate::routing::RoutingTable;
use crate::store::DocumentStore;
use crate::types::{
    AnnotateRequest, BlockRow, DocumentReport, DocumentRow, ObservationRow, PageIssue, PageRow, SearchHit,
};

#[derive(Clone)]
pub struct DocumentState {
    pub store: DocumentStore,
    pub queue: ParseQueue,
    pub lifecycle: Arc<MinerULifecycle>,
    pub routing: RoutingTable,
    pub parse_options: ParseOptions,
    pub desktop: bool,
    /// Set while a worker is draining the queue, so a second parse request joins
    /// the run instead of starting a second worker.
    pub worker_running: Arc<AtomicBool>,
    /// Why the worker last stopped early. Cleared when a parse request starts.
    pub pause_reason: Arc<tokio::sync::Mutex<Option<String>>>,
}

/// Queue totals plus the reason the worker stopped early, when it did.
#[derive(Serialize)]
pub struct ProgressResponse {
    #[serde(flatten)]
    pub queue: QueueProgress,
    pub paused: Option<String>,
}

#[derive(Serialize)]
pub struct ParseResponse {
    pub document: DocumentReport,
    pub parsed: u32,
    pub resumed: u32,
    pub paused: Option<String>,
    pub progress: QueueProgress,
}

pub fn document_routes(state: DocumentState) -> Router {
    Router::new()
        .route("/api/documents", get(list_documents))
        .route("/api/documents/ingest", post(ingest_one))
        .route("/api/documents/scan", post(scan_project))
        .route("/api/documents/search", get(search))
        .route("/api/documents/golden", get(list_golden))
        .route("/api/documents/{id}/report", get(report))
        .route("/api/documents/{id}/pages", get(pages))
        .route("/api/documents/{id}/blocks", get(blocks))
        .route("/api/documents/{id}/issues", get(issues))
        .route("/api/documents/progress", get(progress))
        .route("/api/documents/{id}/parse", post(parse_one))
        .route("/api/documents/{id}/delete", post(delete_one))
        .route("/api/documents/{id}/rename", post(rename_one))
        .route("/api/documents/observations", post(add_observation))
        .route("/api/records-review/from-store", post(review_from_store))
        .with_state(state)
}

#[derive(Deserialize)]
struct ProjectQuery {
    project_id: String,
}

#[derive(Deserialize)]
struct SearchQuery {
    project_id: String,
    q: String,
}

#[derive(Deserialize)]
struct IngestBody {
    project_id: String,
    path: String,
}

#[derive(Deserialize)]
struct ScanBody {
    project_id: String,
    folder: String,
}

#[derive(Deserialize)]
struct RenameBody {
    path: String,
}

#[derive(Deserialize)]
struct PageQuery {
    page_no: u32,
}

#[derive(Deserialize)]
struct ProgressQuery {
    #[serde(default)]
    doc_id: Option<String>,
}

async fn list_documents(
    State(state): State<DocumentState>,
    Query(q): Query<ProjectQuery>,
) -> Result<Json<ApiResponse<Vec<DocumentRow>>>, ApiError> {
    let rows = state
        .store
        .list_project(&q.project_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(ApiResponse::ok(rows)))
}

async fn ingest_one(
    State(state): State<DocumentState>,
    Json(body): Json<IngestBody>,
) -> Result<Json<ApiResponse<DocumentRow>>, ApiError> {
    let limits = if state.desktop {
        IngestLimits::desktop()
    } else {
        IngestLimits::web()
    };
    let row = add_file(&state.store, &body.project_id, &PathBuf::from(body.path), limits)
        .await
        .map_err(ingest_error)?;
    Ok(Json(ApiResponse::ok(row)))
}

async fn scan_project(
    State(state): State<DocumentState>,
    Json(body): Json<ScanBody>,
) -> Result<Json<ApiResponse<Vec<DocumentRow>>>, ApiError> {
    let limits = if state.desktop {
        IngestLimits::desktop()
    } else {
        IngestLimits::web()
    };
    let rows = scan_folder(&state.store, &body.project_id, &PathBuf::from(body.folder), limits)
        .await
        .map_err(ingest_error)?;
    Ok(Json(ApiResponse::ok(rows)))
}

async fn report(
    State(state): State<DocumentState>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<DocumentReport>>, ApiError> {
    let doc = state
        .store
        .find_by_id(&id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .ok_or_else(|| ApiError::NotFound("document".into()))?;
    let report = state
        .store
        .report(&doc)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(ApiResponse::ok(report)))
}

async fn pages(
    State(state): State<DocumentState>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<Vec<PageRow>>>, ApiError> {
    let doc = state
        .store
        .find_by_id(&id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .ok_or_else(|| ApiError::NotFound("document".into()))?;
    let pages = state
        .store
        .list_pages(&doc.sha256)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(ApiResponse::ok(pages)))
}

async fn blocks(
    State(state): State<DocumentState>,
    Path(id): Path<String>,
    Query(q): Query<PageQuery>,
) -> Result<Json<ApiResponse<Vec<BlockRow>>>, ApiError> {
    let doc = state
        .store
        .find_by_id(&id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .ok_or_else(|| ApiError::NotFound("document".into()))?;
    let blocks = state
        .store
        .list_blocks(&doc.sha256, q.page_no)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(ApiResponse::ok(blocks)))
}

async fn issues(
    State(state): State<DocumentState>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<Vec<PageIssue>>>, ApiError> {
    let doc = state
        .store
        .find_by_id(&id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .ok_or_else(|| ApiError::NotFound("document".into()))?;
    let issues = state
        .store
        .list_issues(&doc.sha256)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(ApiResponse::ok(issues)))
}

/// Queue totals for the whole run, or for one document when `doc_id` is given.
/// Polled while a parse is in flight, so the page can show what is left without
/// holding a request open for the length of the run.
async fn progress(
    State(state): State<DocumentState>,
    Query(q): Query<ProgressQuery>,
) -> Result<Json<ApiResponse<ProgressResponse>>, ApiError> {
    let queue = state
        .queue
        .progress(q.doc_id.as_deref())
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let paused = state.pause_reason.lock().await.clone();
    Ok(Json(ApiResponse::ok(ProgressResponse { queue, paused })))
}

async fn parse_one(
    State(state): State<DocumentState>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<ParseResponse>>, ApiError> {
    if state.desktop {
        state.lifecycle.ensure_started().await.map_err(ApiError::BadRequest)?;
        let doc = state
            .store
            .find_by_id(&id)
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?
            .ok_or_else(|| ApiError::NotFound("document".into()))?;
        state
            .queue
            .enqueue_document(&doc.doc_id, &doc.sha256, doc.page_count)
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        *state.pause_reason.lock().await = None;
        spawn_worker(&state);
        let progress = state
            .queue
            .progress(Some(&id))
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        let document = state
            .store
            .report(&doc)
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        return Ok(Json(ApiResponse::ok(ParseResponse {
            document,
            parsed: 0,
            resumed: 0,
            paused: None,
            progress,
        })));
    }
    let _cloud = CloudParseProvider {
        endpoint: String::new(),
    };
    Err(ApiError::BadRequest("desktop parse uses the local engine only".into()))
}

/// Start the single queue worker if one is not already draining. The model
/// parses one page at a time, so a second worker would only add waiting.
fn spawn_worker(state: &DocumentState) {
    if state
        .worker_running
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return;
    }
    let state = state.clone();
    tokio::spawn(async move {
        let provider = LocalDoclibProvider::unix(state.lifecycle.config().socket.clone());
        crate::worker::run_queue(&state, &provider).await;
    });
}

async fn delete_one(
    State(state): State<DocumentState>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<()>>, ApiError> {
    apply_fs_event(
        &state.store,
        "",
        FsChange::Delete(PathBuf::new()),
        IngestLimits::desktop(),
    )
    .await
    .ok();
    state
        .store
        .mark_deleted(&id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(ApiResponse::ok(())))
}

async fn rename_one(
    State(state): State<DocumentState>,
    Path(id): Path<String>,
    Json(body): Json<RenameBody>,
) -> Result<Json<ApiResponse<()>>, ApiError> {
    let name = std::path::Path::new(&body.path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("document")
        .to_string();
    state
        .store
        .rename(&id, &body.path, &name)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(ApiResponse::ok(())))
}

async fn add_observation(
    State(state): State<DocumentState>,
    Json(req): Json<AnnotateRequest>,
) -> Result<Json<ApiResponse<ObservationRow>>, ApiError> {
    let row = annotate(&state.store, req)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(ApiResponse::ok(row)))
}

async fn search(
    State(state): State<DocumentState>,
    Query(q): Query<SearchQuery>,
) -> Result<Json<ApiResponse<Vec<SearchHit>>>, ApiError> {
    let hits = search_project(&state.store, &q.project_id, &q.q)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(ApiResponse::ok(hits)))
}

async fn list_golden() -> Json<ApiResponse<Vec<GoldenField>>> {
    Json(ApiResponse::ok(seed_41744()))
}

async fn review_from_store(
    State(state): State<DocumentState>,
    Json(body): Json<ReviewFromStore>,
) -> Result<Json<ApiResponse<aionui_records_review::ComponentReview>>, ApiError> {
    let pkg = package_from_store(&state.store, &body.project_id, body.package.unwrap_or_default())
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let today = chrono::Local::now().date_naive();
    Ok(Json(ApiResponse::ok(review(&pkg, today))))
}

#[derive(Deserialize)]
struct ReviewFromStore {
    project_id: String,
    #[serde(default)]
    package: Option<ReviewPackage>,
}

fn ingest_error(err: crate::ingest::IngestError) -> ApiError {
    match err {
        crate::ingest::IngestError::Quota(msg) | crate::ingest::IngestError::Unsupported(msg) => {
            ApiError::BadRequest(msg)
        }
        other => ApiError::Internal(other.to_string()),
    }
}
