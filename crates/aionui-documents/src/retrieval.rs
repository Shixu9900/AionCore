use sqlx::Row;

use crate::store::DocumentStore;
use crate::types::SearchHit;

pub async fn search_project(store: &DocumentStore, project_id: &str, query: &str) -> Result<Vec<SearchHit>, sqlx::Error> {
    if query.trim().is_empty() {
        return Ok(Vec::new());
    }
    let rows = sqlx::query(
        "SELECT d.doc_id, d.project_id, d.sha256, d.filename, f.page_no, f.block_id, f.text
         FROM document_blocks_fts f
         JOIN documents d ON d.sha256 = f.sha256
         WHERE document_blocks_fts MATCH ?
           AND d.project_id = ?
           AND d.deleted_at IS NULL
           AND d.superseded_by IS NULL
         LIMIT 20",
    )
    .bind(query)
    .bind(project_id)
    .fetch_all(store.pool())
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| SearchHit {
            project_id: row.get("project_id"),
            doc_id: row.get("doc_id"),
            sha256: row.get("sha256"),
            filename: row.get("filename"),
            page_no: row.get::<i64, _>("page_no") as u32,
            block_id: row.get("block_id"),
            snippet: row.get("text"),
        })
        .collect())
}
