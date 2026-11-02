//! Page-level work queue with expiring leases.
//!
//! The queue exists so a run survives being interrupted. Progress is durable per
//! page, a claim is a lease rather than a lock, and a lost worker costs at most
//! one lease period instead of the whole document.

use aionui_db::SqlitePool;
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use uuid::Uuid;

/// How long a worker holds a page before another worker may take it. Long enough
/// that a slow page is not stolen mid-parse, short enough that a killed process
/// does not strand work for the rest of the run.
pub const LEASE_SECONDS: i64 = 30 * 60;

/// A page that fails this many times is recorded and skipped. The run continues:
/// one unreadable page must never end a ten-thousand-page document.
pub const MAX_ATTEMPTS: i64 = 3;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParseTask {
    pub task_id: String,
    pub doc_id: String,
    pub sha256: String,
    pub page_no: u32,
    pub attempts: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QueueProgress {
    pub total: u32,
    pub pending: u32,
    pub leased: u32,
    pub done: u32,
    pub failed: u32,
}

impl QueueProgress {
    pub fn finished(&self) -> u32 {
        self.done + self.failed
    }

    pub fn is_complete(&self) -> bool {
        self.total > 0 && self.finished() == self.total
    }
}

#[derive(Clone)]
pub struct ParseQueue {
    pool: SqlitePool,
}

impl ParseQueue {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Queue every page of a document that has not already reached a terminal
    /// state. Re-running this on a half-parsed document is how a run resumes, so
    /// it must not reset pages that are already done.
    pub async fn enqueue_document(&self, doc_id: &str, sha256: &str, page_count: u32) -> Result<u32, sqlx::Error> {
        let now = Utc::now().to_rfc3339();
        let mut queued = 0;
        for page_no in 1..=page_count {
            let status: Option<String> =
                sqlx::query_scalar("SELECT status FROM pages WHERE sha256 = ? AND page_no = ?")
                    .bind(sha256)
                    .bind(i64::from(page_no))
                    .fetch_optional(&self.pool)
                    .await?;
            if matches!(status.as_deref(), Some("passed") | Some("doubtful")) {
                continue;
            }
            let affected = sqlx::query(
                "INSERT INTO parse_tasks (task_id, doc_id, sha256, page_no, status, attempts, enqueued_at, updated_at)
                 VALUES (?, ?, ?, ?, 'pending', 0, ?, ?)
                 ON CONFLICT(sha256, page_no) DO UPDATE
                    SET status = 'pending', attempts = 0, last_error = NULL, updated_at = excluded.updated_at
                  WHERE parse_tasks.status IN ('done', 'failed')",
            )
            .bind(Uuid::now_v7().to_string())
            .bind(doc_id)
            .bind(sha256)
            .bind(i64::from(page_no))
            .bind(&now)
            .bind(&now)
            .execute(&self.pool)
            .await?
            .rows_affected();
            queued += affected as u32;
        }
        Ok(queued)
    }

    /// Take up to `limit` pages. Expired leases are claimable, which is the whole
    /// of the crash recovery: a worker that never came back is just a lease that
    /// ran out.
    pub async fn claim(&self, owner: &str, limit: u32) -> Result<Vec<ParseTask>, sqlx::Error> {
        let now = Utc::now();
        let expires = (now + Duration::seconds(LEASE_SECONDS)).to_rfc3339();
        let rows = sqlx::query(
            "UPDATE parse_tasks
                SET status = 'leased', lease_owner = ?, lease_expires_at = ?, updated_at = ?
              WHERE task_id IN (
                    SELECT task_id FROM parse_tasks
                     WHERE status = 'pending'
                        OR (status = 'leased' AND lease_expires_at < ?)
                     ORDER BY enqueued_at
                     LIMIT ?
              )
              RETURNING task_id, doc_id, sha256, page_no, attempts",
        )
        .bind(owner)
        .bind(&expires)
        .bind(now.to_rfc3339())
        .bind(now.to_rfc3339())
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| ParseTask {
                task_id: row.get("task_id"),
                doc_id: row.get("doc_id"),
                sha256: row.get("sha256"),
                page_no: row.get::<i64, _>("page_no") as u32,
                attempts: row.get::<i64, _>("attempts") as u32,
            })
            .collect())
    }

    pub async fn complete(&self, task_id: &str) -> Result<(), sqlx::Error> {
        sqlx::query(
            "UPDATE parse_tasks SET status = 'done', lease_owner = NULL, lease_expires_at = NULL, updated_at = ?
             WHERE task_id = ?",
        )
        .bind(Utc::now().to_rfc3339())
        .bind(task_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Charge an attempt. Below the cap the page goes back in line; at the cap it
    /// is marked failed so the run moves on instead of retrying forever.
    pub async fn fail(&self, task_id: &str, error: &str) -> Result<(), sqlx::Error> {
        sqlx::query(
            "UPDATE parse_tasks
                SET attempts = attempts + 1,
                    status = CASE WHEN attempts + 1 >= ? THEN 'failed' ELSE 'pending' END,
                    last_error = ?,
                    lease_owner = NULL,
                    lease_expires_at = NULL,
                    updated_at = ?
              WHERE task_id = ?",
        )
        .bind(MAX_ATTEMPTS)
        .bind(error)
        .bind(Utc::now().to_rfc3339())
        .bind(task_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Hand a page back without charging an attempt. Used when the engine is down:
    /// the page is fine, the run is simply paused, and nothing about it should
    /// count against the retry budget.
    pub async fn release(&self, task_id: &str) -> Result<(), sqlx::Error> {
        sqlx::query(
            "UPDATE parse_tasks SET status = 'pending', lease_owner = NULL, lease_expires_at = NULL, updated_at = ?
             WHERE task_id = ?",
        )
        .bind(Utc::now().to_rfc3339())
        .bind(task_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Hand back every page `owner` still holds. Only safe when `owner` is known
    /// not to be working, such as a worker starting after the app was killed.
    pub async fn release_owner(&self, owner: &str) -> Result<(), sqlx::Error> {
        sqlx::query(
            "UPDATE parse_tasks SET status = 'pending', lease_owner = NULL, lease_expires_at = NULL, updated_at = ?
             WHERE status = 'leased' AND lease_owner = ?",
        )
        .bind(Utc::now().to_rfc3339())
        .bind(owner)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn progress(&self, doc_id: Option<&str>) -> Result<QueueProgress, sqlx::Error> {
        let rows = match doc_id {
            Some(id) => {
                sqlx::query("SELECT status, COUNT(*) AS n FROM parse_tasks WHERE doc_id = ? GROUP BY status")
                    .bind(id)
                    .fetch_all(&self.pool)
                    .await?
            }
            None => {
                sqlx::query("SELECT status, COUNT(*) AS n FROM parse_tasks GROUP BY status")
                    .fetch_all(&self.pool)
                    .await?
            }
        };
        let mut progress = QueueProgress::default();
        for row in rows {
            let count = row.get::<i64, _>("n") as u32;
            progress.total += count;
            match row.get::<String, _>("status").as_str() {
                "pending" => progress.pending += count,
                "leased" => progress.leased += count,
                "done" => progress.done += count,
                "failed" => progress.failed += count,
                _ => {}
            }
        }
        Ok(progress)
    }
}
