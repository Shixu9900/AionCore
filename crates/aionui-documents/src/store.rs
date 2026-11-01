use aionui_db::SqlitePool;
use chrono::Utc;
use sqlx::Row;
use uuid::Uuid;

use crate::types::{BlockRow, DocumentReport, DocumentRow, ObservationRow, PageIssue, PageRow};

#[derive(Clone)]
pub struct DocumentStore {
    pool: SqlitePool,
}

impl DocumentStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    pub async fn upsert_document(&self, row: &DocumentRow) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO documents (doc_id, project_id, sha256, path, filename, page_count, bytes, added_at, deleted_at, superseded_by)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(doc_id) DO UPDATE SET
                sha256 = excluded.sha256,
                path = excluded.path,
                filename = excluded.filename,
                page_count = excluded.page_count,
                bytes = excluded.bytes,
                deleted_at = excluded.deleted_at,
                superseded_by = excluded.superseded_by",
        )
        .bind(&row.doc_id)
        .bind(&row.project_id)
        .bind(&row.sha256)
        .bind(&row.path)
        .bind(&row.filename)
        .bind(row.page_count as i64)
        .bind(row.bytes as i64)
        .bind(&row.added_at)
        .bind(&row.deleted_at)
        .bind(&row.superseded_by)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn find_by_path(&self, project_id: &str, path: &str) -> Result<Option<DocumentRow>, sqlx::Error> {
        let row = sqlx::query(
            "SELECT doc_id, project_id, sha256, path, filename, page_count, bytes, added_at, deleted_at, superseded_by
             FROM documents WHERE project_id = ? AND path = ? AND deleted_at IS NULL AND superseded_by IS NULL",
        )
        .bind(project_id)
        .bind(path)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(map_document))
    }

    pub async fn find_by_id(&self, doc_id: &str) -> Result<Option<DocumentRow>, sqlx::Error> {
        let row = sqlx::query(
            "SELECT doc_id, project_id, sha256, path, filename, page_count, bytes, added_at, deleted_at, superseded_by
             FROM documents WHERE doc_id = ?",
        )
        .bind(doc_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(map_document))
    }

    pub async fn list_project(&self, project_id: &str) -> Result<Vec<DocumentRow>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT doc_id, project_id, sha256, path, filename, page_count, bytes, added_at, deleted_at, superseded_by
             FROM documents WHERE project_id = ? AND deleted_at IS NULL ORDER BY filename",
        )
        .bind(project_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(map_document).collect())
    }

    pub async fn pages_complete(&self, sha256: &str) -> Result<bool, sqlx::Error> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM pages WHERE sha256 = ? AND status != 'pending'")
            .bind(sha256)
            .fetch_one(&self.pool)
            .await?;
        Ok(count > 0)
    }

    pub async fn upsert_page(&self, page: &PageRow) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO pages (sha256, page_no, kind, rotation, skew_degrees, text_chars, image_coverage, garbled_ratio, status, confidence, provider)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(sha256, page_no) DO UPDATE SET
                kind = excluded.kind,
                rotation = excluded.rotation,
                skew_degrees = excluded.skew_degrees,
                text_chars = excluded.text_chars,
                image_coverage = excluded.image_coverage,
                garbled_ratio = excluded.garbled_ratio,
                status = excluded.status,
                confidence = excluded.confidence,
                provider = excluded.provider",
        )
        .bind(&page.sha256)
        .bind(page.page_no as i64)
        .bind(&page.kind)
        .bind(page.rotation as i64)
        .bind(page.skew_degrees)
        .bind(page.text_chars as i64)
        .bind(page.image_coverage)
        .bind(page.garbled_ratio)
        .bind(&page.status)
        .bind(page.confidence)
        .bind(&page.provider)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn list_pages(&self, sha256: &str) -> Result<Vec<PageRow>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT sha256, page_no, kind, rotation, skew_degrees, text_chars, image_coverage, garbled_ratio, status, confidence, provider
             FROM pages WHERE sha256 = ? ORDER BY page_no",
        )
        .bind(sha256)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(map_page).collect())
    }

    pub async fn replace_blocks(&self, sha256: &str, page_no: u32, blocks: &[BlockRow]) -> Result<(), sqlx::Error> {
        sqlx::query("DELETE FROM document_blocks_fts WHERE sha256 = ? AND page_no = ?")
            .bind(sha256)
            .bind(page_no as i64)
            .execute(&self.pool)
            .await?;
        sqlx::query("DELETE FROM blocks WHERE sha256 = ? AND page_no = ?")
            .bind(sha256)
            .bind(page_no as i64)
            .execute(&self.pool)
            .await?;
        for block in blocks {
            sqlx::query(
                "INSERT INTO blocks (block_id, sha256, page_no, label, bbox, text, html, confidence, reading_order, provider)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&block.block_id)
            .bind(&block.sha256)
            .bind(block.page_no as i64)
            .bind(&block.label)
            .bind(block.bbox.map(bbox_to_json))
            .bind(&block.text)
            .bind(&block.html)
            .bind(block.confidence)
            .bind(block.reading_order.map(i64::from))
            .bind(&block.provider)
            .execute(&self.pool)
            .await?;
            sqlx::query("INSERT INTO document_blocks_fts (text, sha256, page_no, block_id) VALUES (?, ?, ?, ?)")
                .bind(&block.text)
                .bind(&block.sha256)
                .bind(block.page_no as i64)
                .bind(&block.block_id)
                .execute(&self.pool)
                .await?;
        }
        Ok(())
    }

    pub async fn list_blocks(&self, sha256: &str, page_no: u32) -> Result<Vec<BlockRow>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT block_id, sha256, page_no, label, bbox, text, html, confidence, reading_order, provider
             FROM blocks WHERE sha256 = ? AND page_no = ? ORDER BY reading_order",
        )
        .bind(sha256)
        .bind(page_no as i64)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(map_block).collect())
    }

    pub async fn insert_issue(&self, issue: &PageIssue) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO page_issues (issue_id, sha256, page_no, bbox, code, severity, detail) VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&issue.issue_id)
        .bind(&issue.sha256)
        .bind(issue.page_no as i64)
        .bind(issue.bbox.map(bbox_to_json))
        .bind(&issue.code)
        .bind(&issue.severity)
        .bind(&issue.detail)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn clear_issues(&self, sha256: &str, page_no: u32) -> Result<(), sqlx::Error> {
        sqlx::query("DELETE FROM page_issues WHERE sha256 = ? AND page_no = ?")
            .bind(sha256)
            .bind(page_no as i64)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn list_issues(&self, sha256: &str) -> Result<Vec<PageIssue>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT issue_id, sha256, page_no, bbox, code, severity, detail FROM page_issues WHERE sha256 = ? ORDER BY page_no",
        )
        .bind(sha256)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(map_issue).collect())
    }

    pub async fn upsert_observation(&self, row: &ObservationRow) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO observations (obs_id, project_id, sha256, page_no, bbox, metric, raw, value, confidence, source, status, block_id, superseded_by, confirmed_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(obs_id) DO UPDATE SET
                raw = excluded.raw,
                value = excluded.value,
                confidence = excluded.confidence,
                source = excluded.source,
                status = excluded.status,
                bbox = excluded.bbox,
                superseded_by = excluded.superseded_by,
                confirmed_at = excluded.confirmed_at",
        )
        .bind(&row.obs_id)
        .bind(&row.project_id)
        .bind(&row.sha256)
        .bind(row.page_no as i64)
        .bind(row.bbox.map(bbox_to_json))
        .bind(&row.metric)
        .bind(&row.raw)
        .bind(row.value)
        .bind(row.confidence)
        .bind(&row.source)
        .bind(&row.status)
        .bind(&row.block_id)
        .bind(&row.superseded_by)
        .bind(&row.confirmed_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn list_observations(&self, project_id: &str, sha256: Option<&str>) -> Result<Vec<ObservationRow>, sqlx::Error> {
        let rows = if let Some(sha) = sha256 {
            sqlx::query(
                "SELECT obs_id, project_id, sha256, page_no, bbox, metric, raw, value, confidence, source, status, block_id, superseded_by, confirmed_at
                 FROM observations WHERE project_id = ? AND sha256 = ? AND superseded_by IS NULL",
            )
            .bind(project_id)
            .bind(sha)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query(
                "SELECT obs_id, project_id, sha256, page_no, bbox, metric, raw, value, confidence, source, status, block_id, superseded_by, confirmed_at
                 FROM observations WHERE project_id = ? AND superseded_by IS NULL",
            )
            .bind(project_id)
            .fetch_all(&self.pool)
            .await?
        };
        Ok(rows.into_iter().map(map_observation).collect())
    }

    pub async fn mark_deleted(&self, doc_id: &str) -> Result<(), sqlx::Error> {
        let now = Utc::now().to_rfc3339();
        sqlx::query("UPDATE documents SET deleted_at = ? WHERE doc_id = ?")
            .bind(&now)
            .bind(doc_id)
            .execute(&self.pool)
            .await?;
        sqlx::query("UPDATE observations SET status = 'withdrawn' WHERE sha256 = (SELECT sha256 FROM documents WHERE doc_id = ?)")
            .bind(doc_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn rename(&self, doc_id: &str, path: &str, filename: &str) -> Result<(), sqlx::Error> {
        sqlx::query("UPDATE documents SET path = ?, filename = ? WHERE doc_id = ?")
            .bind(path)
            .bind(filename)
            .bind(doc_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn supersede(&self, old_id: &str, new_id: &str) -> Result<(), sqlx::Error> {
        sqlx::query("UPDATE documents SET superseded_by = ? WHERE doc_id = ?")
            .bind(new_id)
            .bind(old_id)
            .execute(&self.pool)
            .await?;
        sqlx::query(
            "UPDATE observations SET status = 'stale' WHERE sha256 = (SELECT sha256 FROM documents WHERE doc_id = ?)",
        )
        .bind(old_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn record_parse_run(
        &self,
        sha256: &str,
        provider: &str,
        tier: &str,
        page_from: u32,
        page_to: u32,
        status: &str,
        error: Option<&str>,
    ) -> Result<String, sqlx::Error> {
        let run_id = Uuid::now_v7().to_string();
        sqlx::query(
            "INSERT INTO parse_runs (run_id, sha256, provider, tier, page_from, page_to, status, error_msg, started_at, finished_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&run_id)
        .bind(sha256)
        .bind(provider)
        .bind(tier)
        .bind(page_from as i64)
        .bind(page_to as i64)
        .bind(status)
        .bind(error)
        .bind(Utc::now().to_rfc3339())
        .bind(Utc::now().to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(run_id)
    }

    pub async fn report(&self, doc: &DocumentRow) -> Result<DocumentReport, sqlx::Error> {
        let pages = self.list_pages(&doc.sha256).await?;
        let issues = self.list_issues(&doc.sha256).await?;
        let mut passed = 0;
        let mut doubtful = 0;
        let mut failed = 0;
        for page in &pages {
            match page.status.as_str() {
                "passed" => passed += 1,
                "failed" => failed += 1,
                _ => doubtful += 1,
            }
        }
        Ok(DocumentReport {
            doc_id: doc.doc_id.clone(),
            sha256: doc.sha256.clone(),
            filename: doc.filename.clone(),
            page_count: doc.page_count,
            passed,
            doubtful,
            failed,
            issues,
            engine: "local-mineru".into(),
        })
    }
}

fn map_document(row: sqlx::sqlite::SqliteRow) -> DocumentRow {
    DocumentRow {
        doc_id: row.get("doc_id"),
        project_id: row.get("project_id"),
        sha256: row.get("sha256"),
        path: row.get("path"),
        filename: row.get("filename"),
        page_count: row.get::<i64, _>("page_count") as u32,
        bytes: row.get::<i64, _>("bytes") as u64,
        added_at: row.get("added_at"),
        deleted_at: row.get("deleted_at"),
        superseded_by: row.get("superseded_by"),
    }
}

fn map_page(row: sqlx::sqlite::SqliteRow) -> PageRow {
    PageRow {
        sha256: row.get("sha256"),
        page_no: row.get::<i64, _>("page_no") as u32,
        kind: row.get("kind"),
        rotation: row.get::<i64, _>("rotation") as i32,
        skew_degrees: row.get("skew_degrees"),
        text_chars: row.get::<i64, _>("text_chars") as u32,
        image_coverage: row.get("image_coverage"),
        garbled_ratio: row.get("garbled_ratio"),
        status: row.get("status"),
        confidence: row.get("confidence"),
        provider: row.get("provider"),
    }
}

fn map_block(row: sqlx::sqlite::SqliteRow) -> BlockRow {
    BlockRow {
        block_id: row.get("block_id"),
        sha256: row.get("sha256"),
        page_no: row.get::<i64, _>("page_no") as u32,
        label: row.get("label"),
        bbox: row.get::<Option<String>, _>("bbox").and_then(|s| serde_json::from_str(&s).ok()),
        text: row.get("text"),
        html: row.get("html"),
        confidence: row.get("confidence"),
        reading_order: row.get::<Option<i64>, _>("reading_order").map(|v| v as i32),
        provider: row.get("provider"),
    }
}

fn map_issue(row: sqlx::sqlite::SqliteRow) -> PageIssue {
    PageIssue {
        issue_id: row.get("issue_id"),
        sha256: row.get("sha256"),
        page_no: row.get::<i64, _>("page_no") as u32,
        bbox: row.get::<Option<String>, _>("bbox").and_then(|s| serde_json::from_str(&s).ok()),
        code: row.get("code"),
        severity: row.get("severity"),
        detail: row.get("detail"),
    }
}

fn map_observation(row: sqlx::sqlite::SqliteRow) -> ObservationRow {
    ObservationRow {
        obs_id: row.get("obs_id"),
        project_id: row.get("project_id"),
        sha256: row.get("sha256"),
        page_no: row.get::<i64, _>("page_no") as u32,
        bbox: row.get::<Option<String>, _>("bbox").and_then(|s| serde_json::from_str(&s).ok()),
        metric: row.get("metric"),
        raw: row.get("raw"),
        value: row.get("value"),
        confidence: row.get("confidence"),
        source: row.get("source"),
        status: row.get("status"),
        block_id: row.get("block_id"),
        superseded_by: row.get("superseded_by"),
        confirmed_at: row.get("confirmed_at"),
    }
}

fn bbox_to_json(bbox: [f64; 4]) -> String {
    serde_json::to_string(&bbox).unwrap_or_else(|_| "[]".into())
}
