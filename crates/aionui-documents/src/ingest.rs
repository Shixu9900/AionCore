use std::path::{Path, PathBuf};

use chrono::Utc;
use thiserror::Error;
use uuid::Uuid;

use crate::store::DocumentStore;
use crate::triage::{classify_path, sha256_file};
use crate::types::{DocumentRow, IssueCode, PageIssue, PageKind};
use crate::{DESKTOP_PAGE_LIMIT, WEB_PAGE_LIMIT};

#[derive(Debug, Error)]
pub enum IngestError {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Db(#[from] sqlx::Error),
    #[error("{0}")]
    Quota(String),
    #[error("{0}")]
    Unsupported(String),
}

#[derive(Debug, Clone, Copy)]
pub struct IngestLimits {
    pub max_pages: u32,
}

impl IngestLimits {
    pub fn desktop() -> Self {
        Self {
            max_pages: DESKTOP_PAGE_LIMIT,
        }
    }

    pub fn web() -> Self {
        Self {
            max_pages: WEB_PAGE_LIMIT,
        }
    }
}

pub enum FsChange {
    Add(PathBuf),
    Delete(PathBuf),
    Rename { from: PathBuf, to: PathBuf },
    Edit(PathBuf),
}

pub async fn add_file(
    store: &DocumentStore,
    project_id: &str,
    path: &Path,
    limits: IngestLimits,
) -> Result<DocumentRow, IngestError> {
    let (sha, bytes, content) = sha256_file(path)?;
    if let Some(existing) = store.find_by_path(project_id, &path_string(path)).await? {
        if existing.sha256 == sha {
            return Ok(existing);
        }
        let new_row = insert_new(store, project_id, path, &sha, bytes, &content, limits).await?;
        store.supersede(&existing.doc_id, &new_row.doc_id).await?;
        return Ok(new_row);
    }
    insert_new(store, project_id, path, &sha, bytes, &content, limits).await
}

async fn insert_new(
    store: &DocumentStore,
    project_id: &str,
    path: &Path,
    sha: &str,
    bytes: u64,
    content: &[u8],
    limits: IngestLimits,
) -> Result<DocumentRow, IngestError> {
    let (page_count, mut pages, _text) = classify_path(path, content);
    if page_count > limits.max_pages {
        return Err(IngestError::Quota(format!(
            "This file has {page_count} pages. The desktop limit is {}. Split the file and add the parts.",
            limits.max_pages
        )));
    }
    let filename = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("document")
        .to_string();
    let row = DocumentRow {
        doc_id: Uuid::now_v7().to_string(),
        project_id: project_id.to_string(),
        sha256: sha.to_string(),
        path: path_string(path),
        filename,
        page_count,
        bytes,
        added_at: Utc::now().to_rfc3339(),
        deleted_at: None,
        superseded_by: None,
    };
    store.upsert_document(&row).await?;
    for page in &mut pages {
        page.sha256 = sha.to_string();
        store.upsert_page(page).await?;
    }
    if path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "doc" | "ppt"))
    {
        store
            .insert_issue(&PageIssue {
                issue_id: Uuid::now_v7().to_string(),
                sha256: sha.to_string(),
                page_no: 1,
                bbox: None,
                code: IssueCode::UnsupportedFormat.as_str().into(),
                severity: "high".into(),
                detail: "Save this file as DOCX, PPTX, or PDF so the assistant can read its structure.".into(),
            })
            .await?;
    }
    if pages.iter().any(|p| PageKind::parse(&p.kind) == PageKind::Unknown) && pages.len() == 1 {
        // Keep unknown pages; they still enter the store so the user can see them.
    }
    Ok(row)
}

pub async fn apply_fs_event(
    store: &DocumentStore,
    project_id: &str,
    change: FsChange,
    limits: IngestLimits,
) -> Result<Option<DocumentRow>, IngestError> {
    match change {
        FsChange::Add(path) | FsChange::Edit(path) => Ok(Some(add_file(store, project_id, &path, limits).await?)),
        FsChange::Delete(path) => {
            if let Some(row) = store.find_by_path(project_id, &path_string(&path)).await? {
                store.mark_deleted(&row.doc_id).await?;
            }
            Ok(None)
        }
        FsChange::Rename { from, to } => {
            if let Some(row) = store.find_by_path(project_id, &path_string(&from)).await? {
                let name = to
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(&row.filename)
                    .to_string();
                store.rename(&row.doc_id, &path_string(&to), &name).await?;
                return store.find_by_id(&row.doc_id).await.map_err(Into::into);
            }
            Ok(Some(add_file(store, project_id, &to, limits).await?))
        }
    }
}

pub async fn scan_folder(
    store: &DocumentStore,
    project_id: &str,
    root: &Path,
    limits: IngestLimits,
) -> Result<Vec<DocumentRow>, IngestError> {
    let mut out = Vec::new();
    for path in walk(root) {
        if is_document(&path) {
            out.push(add_file(store, project_id, &path, limits).await?);
        }
    }
    Ok(out)
}

fn walk(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                files.push(path);
            }
        }
    }
    files
}

fn is_document(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase().as_str(),
        "pdf" | "docx" | "pptx" | "doc" | "ppt" | "xlsx" | "xls" | "csv" | "png" | "jpg" | "jpeg" | "tif" | "tiff" | "webp"
    )
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}
