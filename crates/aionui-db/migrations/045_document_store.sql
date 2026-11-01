-- Document store: content hash is identity. pages/blocks are keyed by sha256
-- so the same file is parsed once across projects. observations carry project_id
-- because a human confirmation belongs to a review, not to the file.

CREATE TABLE documents (
    doc_id TEXT PRIMARY KEY NOT NULL,
    project_id TEXT NOT NULL,
    sha256 TEXT NOT NULL,
    path TEXT NOT NULL,
    filename TEXT NOT NULL,
    page_count INTEGER NOT NULL DEFAULT 0,
    bytes INTEGER NOT NULL DEFAULT 0,
    added_at TEXT NOT NULL,
    deleted_at TEXT,
    superseded_by TEXT
);

CREATE INDEX idx_documents_project ON documents (project_id, deleted_at);
CREATE INDEX idx_documents_sha ON documents (sha256);

CREATE TABLE parse_runs (
    run_id TEXT PRIMARY KEY NOT NULL,
    sha256 TEXT NOT NULL,
    provider TEXT NOT NULL,
    tier TEXT NOT NULL,
    page_from INTEGER NOT NULL,
    page_to INTEGER NOT NULL,
    status TEXT NOT NULL,
    error_code TEXT,
    error_msg TEXT,
    timings TEXT,
    started_at TEXT NOT NULL,
    finished_at TEXT
);

CREATE INDEX idx_parse_runs_sha ON parse_runs (sha256, status);

CREATE TABLE pages (
    sha256 TEXT NOT NULL,
    page_no INTEGER NOT NULL,
    kind TEXT NOT NULL,
    rotation INTEGER NOT NULL DEFAULT 0,
    skew_degrees REAL,
    text_chars INTEGER NOT NULL DEFAULT 0,
    image_coverage REAL NOT NULL DEFAULT 0,
    garbled_ratio REAL NOT NULL DEFAULT 0,
    status TEXT NOT NULL,
    confidence REAL,
    provider TEXT,
    PRIMARY KEY (sha256, page_no)
);

CREATE TABLE blocks (
    block_id TEXT PRIMARY KEY NOT NULL,
    sha256 TEXT NOT NULL,
    page_no INTEGER NOT NULL,
    label TEXT,
    bbox TEXT,
    text TEXT,
    html TEXT,
    confidence REAL,
    reading_order INTEGER,
    provider TEXT
);

CREATE INDEX idx_blocks_page ON blocks (sha256, page_no);

CREATE TABLE observations (
    obs_id TEXT PRIMARY KEY NOT NULL,
    project_id TEXT NOT NULL,
    sha256 TEXT NOT NULL,
    page_no INTEGER NOT NULL,
    bbox TEXT,
    metric TEXT NOT NULL,
    raw TEXT NOT NULL,
    value REAL,
    confidence REAL,
    source TEXT NOT NULL,
    status TEXT NOT NULL,
    block_id TEXT,
    superseded_by TEXT,
    confirmed_at TEXT
);

CREATE INDEX idx_observations_project ON observations (project_id, status);
CREATE INDEX idx_observations_sha ON observations (sha256, page_no);

CREATE TABLE page_issues (
    issue_id TEXT PRIMARY KEY NOT NULL,
    sha256 TEXT NOT NULL,
    page_no INTEGER NOT NULL,
    bbox TEXT,
    code TEXT NOT NULL,
    severity TEXT NOT NULL,
    detail TEXT NOT NULL
);

CREATE INDEX idx_page_issues_page ON page_issues (sha256, page_no);

CREATE VIRTUAL TABLE document_blocks_fts USING fts5 (
    text,
    sha256 UNINDEXED,
    page_no UNINDEXED,
    block_id UNINDEXED
);
