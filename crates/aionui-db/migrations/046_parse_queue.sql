-- Page-level durable work queue. A parse run is tens of thousands of pages, so
-- the restart unit has to be the page, not the document.
--
-- A worker claims a page by writing lease_owner and lease_expires_at. If the
-- process is killed, the machine sleeps, or the assistant quits, the lease simply
-- expires and the next worker takes the page. There is no crash-recovery pass to
-- write and none to get wrong.

CREATE TABLE parse_tasks (
    task_id TEXT PRIMARY KEY NOT NULL,
    doc_id TEXT NOT NULL,
    sha256 TEXT NOT NULL,
    page_no INTEGER NOT NULL,
    status TEXT NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    lease_owner TEXT,
    lease_expires_at TEXT,
    last_error TEXT,
    enqueued_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

-- Identity is the page, not the task row: enqueueing a document twice must not
-- queue its pages twice.
CREATE UNIQUE INDEX idx_parse_tasks_page ON parse_tasks (sha256, page_no);
CREATE INDEX idx_parse_tasks_claim ON parse_tasks (status, lease_expires_at);
CREATE INDEX idx_parse_tasks_doc ON parse_tasks (doc_id, status);
