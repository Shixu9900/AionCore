use std::io::Write;

use aionui_db::init_database_memory;
use aionui_documents::{
    AnnotateRequest, DocumentState, DocumentStore, IngestLimits, MAX_ATTEMPTS, MinerULifecycle, ParseOptions,
    ParseQueue, RoutingTable, add_file, annotate, field_exact_match, package_from_store, parse_document, replay,
    run_queue, search_project, seed_41744,
};
use aionui_documents::{FsChange, ParseProvider, ParseRequest, ProviderError, ReaderOutput, apply_fs_event};
use async_trait::async_trait;

struct FakeProvider {
    text: String,
}

#[async_trait]
impl ParseProvider for FakeProvider {
    async fn parse(&self, request: &ParseRequest) -> Result<ReaderOutput, ProviderError> {
        if request.remote {
            return Err(ProviderError::CloudDisabled);
        }
        Ok(ReaderOutput {
            provider: format!("mineru-{}", request.tier.as_str()),
            tier: Some(request.tier.as_str().into()),
            text: self.text.clone(),
            blocks: Vec::new(),
            fields: Vec::new(),
            confidence: 0.9,
        })
    }
}

async fn store() -> DocumentStore {
    let db = init_database_memory().await.expect("memory db");
    DocumentStore::new(db.pool().clone())
}

#[tokio::test]
async fn ingest_add_rename_delete_and_edit() {
    let store = store().await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("blade.txt");
    std::fs::write(&path, "TSN 29189 CSN 4585").unwrap();

    let added = add_file(&store, "proj-a", &path, IngestLimits::desktop())
        .await
        .unwrap();
    assert_eq!(added.page_count, 1);
    let same = add_file(&store, "proj-a", &path, IngestLimits::desktop())
        .await
        .unwrap();
    assert_eq!(same.doc_id, added.doc_id);

    let renamed = dir.path().join("blade-moved.txt");
    apply_fs_event(
        &store,
        "proj-a",
        FsChange::Rename {
            from: path.clone(),
            to: renamed.clone(),
        },
        IngestLimits::desktop(),
    )
    .await
    .unwrap();
    let after_rename = store.find_by_id(&added.doc_id).await.unwrap().unwrap();
    assert!(after_rename.path.ends_with("blade-moved.txt"));

    std::fs::write(&renamed, "TSN 29189 CSN 4586").unwrap();
    let edited = add_file(&store, "proj-a", &renamed, IngestLimits::desktop())
        .await
        .unwrap();
    assert_ne!(edited.sha256, added.sha256);
    assert_eq!(
        store
            .find_by_id(&added.doc_id)
            .await
            .unwrap()
            .unwrap()
            .superseded_by
            .as_deref(),
        Some(edited.doc_id.as_str())
    );

    apply_fs_event(&store, "proj-a", FsChange::Delete(renamed), IngestLimits::desktop())
        .await
        .unwrap();
    assert!(
        store
            .find_by_id(&edited.doc_id)
            .await
            .unwrap()
            .unwrap()
            .deleted_at
            .is_some()
    );
}

/// Page count must not decide whether a file is allowed in. A lease return is
/// tens of thousands of pages and the page queue is what carries it, so the old
/// behaviour here — refusing the file outright — was the wrong shape of limit.
#[tokio::test]
async fn ingest_does_not_refuse_a_file_for_its_page_count() {
    let store = store().await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("big.txt");
    std::fs::write(&path, "x").unwrap();
    for limits in [IngestLimits::desktop(), IngestLimits::trial(), IngestLimits::web()] {
        add_file(&store, "p", &path, limits)
            .await
            .expect("accepted regardless of budget");
    }
}

/// An interrupted run resumes: pages finished earlier are not read again.
#[tokio::test]
async fn a_second_parse_resumes_instead_of_rereading() {
    let store = store().await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("note.txt");
    std::fs::write(&path, "TSN 29189").unwrap();
    let doc = add_file(&store, "proj-a", &path, IngestLimits::desktop())
        .await
        .unwrap();
    let provider = FakeProvider {
        text: "TSN 29189".into(),
    };

    let first = parse_document(
        &store,
        &doc.doc_id,
        &provider,
        &RoutingTable::default(),
        ParseOptions::default(),
    )
    .await
    .unwrap();
    assert_eq!(first.parsed, 1);
    assert_eq!(first.resumed, 0);

    let second = parse_document(
        &store,
        &doc.doc_id,
        &provider,
        &RoutingTable::default(),
        ParseOptions::default(),
    )
    .await
    .unwrap();
    assert_eq!(second.parsed, 0, "the finished page must not be read a second time");
    assert_eq!(second.resumed, 1);
}

/// The queue is the crash recovery. A worker that never comes back leaves an
/// expired lease, and the next claim takes the page: no recovery pass, no
/// bookkeeping to get wrong.
#[tokio::test]
async fn an_expired_lease_returns_the_page_to_the_queue() {
    let db = init_database_memory().await.expect("memory db");
    let queue = ParseQueue::new(db.pool().clone());
    queue.enqueue_document("doc-1", "sha-1", 3).await.unwrap();

    let held = queue.claim("worker-a", 10).await.unwrap();
    assert_eq!(held.len(), 3);
    assert!(
        queue.claim("worker-b", 10).await.unwrap().is_empty(),
        "a live lease must not be stealable"
    );

    sqlx::query("UPDATE parse_tasks SET lease_expires_at = '2000-01-01T00:00:00Z'")
        .execute(db.pool())
        .await
        .unwrap();
    assert_eq!(
        queue.claim("worker-b", 10).await.unwrap().len(),
        3,
        "an expired lease must be claimable by the next worker"
    );

    queue.complete(&held[0].task_id).await.unwrap();
    let progress = queue.progress(Some("doc-1")).await.unwrap();
    assert_eq!(progress.total, 3);
    assert_eq!(progress.done, 1);
}

/// A page that keeps failing is recorded and skipped. The run has to outlive its
/// worst page, not stop at it.
#[tokio::test]
async fn a_page_is_dropped_after_its_retries_run_out() {
    let db = init_database_memory().await.expect("memory db");
    let queue = ParseQueue::new(db.pool().clone());
    queue.enqueue_document("doc-1", "sha-1", 1).await.unwrap();

    for _ in 0..MAX_ATTEMPTS {
        let task = queue.claim("worker-a", 1).await.unwrap();
        assert_eq!(task.len(), 1, "a page under its retry budget stays claimable");
        queue.fail(&task[0].task_id, "render failed").await.unwrap();
    }
    assert!(queue.claim("worker-a", 1).await.unwrap().is_empty());
    assert_eq!(queue.progress(Some("doc-1")).await.unwrap().failed, 1);

    queue.enqueue_document("doc-1", "sha-1", 1).await.unwrap();
    let retry = queue.claim("worker-a", 1).await.unwrap();
    assert_eq!(retry.len(), 1, "asking to parse again must retry a page that ran out of retries");
    assert_eq!(retry[0].attempts, 0);
}

/// A killed app leaves its page leased. The next worker takes it back at once
/// instead of waiting out the lease.
#[tokio::test]
async fn a_restarted_worker_takes_back_its_own_lease() {
    let db = init_database_memory().await.expect("memory db");
    let queue = ParseQueue::new(db.pool().clone());
    queue.enqueue_document("doc-1", "sha-1", 1).await.unwrap();
    assert_eq!(queue.claim("aionui", 1).await.unwrap().len(), 1);
    assert!(queue.claim("aionui", 1).await.unwrap().is_empty());

    queue.release_owner("other").await.unwrap();
    assert!(queue.claim("aionui", 1).await.unwrap().is_empty());
    queue.release_owner("aionui").await.unwrap();
    assert_eq!(queue.claim("aionui", 1).await.unwrap().len(), 1);
}

fn worker_state(store: DocumentStore, queue: ParseQueue, dir: &std::path::Path) -> DocumentState {
    DocumentState {
        store,
        queue,
        lifecycle: std::sync::Arc::new(MinerULifecycle::new(dir.to_path_buf())),
        routing: RoutingTable::default(),
        parse_options: ParseOptions::default(),
        desktop: true,
        worker_running: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
        pause_reason: std::sync::Arc::new(tokio::sync::Mutex::new(None)),
    }
}

async fn run_worker_once<P: ParseProvider>(
    provider: &P,
    body: &str,
) -> (aionui_documents::QueueProgress, Option<String>) {
    let db = init_database_memory().await.expect("memory db");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("note.txt");
    std::fs::write(&path, body).unwrap();
    let store = DocumentStore::new(db.pool().clone());
    let doc = add_file(&store, "p", &path, IngestLimits::desktop()).await.unwrap();
    let queue = ParseQueue::new(db.pool().clone());
    queue
        .enqueue_document(&doc.doc_id, &doc.sha256, doc.page_count)
        .await
        .unwrap();
    let state = worker_state(store, queue, dir.path());
    run_queue(&state, provider).await;
    let progress = state.queue.progress(Some(&doc.doc_id)).await.unwrap();
    let paused = state.pause_reason.lock().await.clone();
    (progress, paused)
}

/// The worker is the whole of the orchestration: it takes a page, records it, and
/// marks the queue done. When the engine is down it hands the page back with its
/// retries intact and says why it stopped.
#[tokio::test]
async fn the_worker_finishes_a_page_and_releases_it_when_the_engine_is_down() {
    let (done, paused) = run_worker_once(
        &FakeProvider {
            text: "TSN 29189".into(),
        },
        "TSN 29189",
    )
    .await;
    assert_eq!(done.done, 1, "a page the engine read must leave the queue");
    assert!(paused.is_none());

    struct Down;
    #[async_trait]
    impl ParseProvider for Down {
        async fn parse(&self, _request: &ParseRequest) -> Result<ReaderOutput, ProviderError> {
            Err(ProviderError::Unavailable)
        }
    }
    let (released, paused) = run_worker_once(&Down, "TSN 11111").await;
    assert_eq!(released.pending, 1, "an engine outage hands the page back");
    assert_eq!(released.failed, 0, "an engine outage must not spend a retry");
    assert!(paused.is_some());
}

/// Releasing is not failing. The engine going down says nothing about the page,
/// so it must not spend a retry.
#[tokio::test]
async fn releasing_a_page_does_not_charge_a_retry() {
    let db = init_database_memory().await.expect("memory db");
    let queue = ParseQueue::new(db.pool().clone());
    queue.enqueue_document("doc-1", "sha-1", 1).await.unwrap();

    for _ in 0..MAX_ATTEMPTS + 2 {
        let task = queue.claim("worker-a", 1).await.unwrap();
        assert_eq!(task.len(), 1);
        assert_eq!(task[0].attempts, 0);
        queue.release(&task[0].task_id).await.unwrap();
    }
}

#[tokio::test]
async fn dual_readers_and_local_only() {
    let store = store().await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("note.txt");
    std::fs::write(&path, "TSN 29189").unwrap();
    let doc = add_file(&store, "proj-a", &path, IngestLimits::desktop())
        .await
        .unwrap();
    let provider = FakeProvider {
        text: "TSN 29189".into(),
    };
    let report = parse_document(
        &store,
        &doc.doc_id,
        &provider,
        &RoutingTable::default(),
        ParseOptions::default(),
    )
    .await
    .unwrap();
    assert_eq!(report.document.page_count, 1);

    struct RemoteOnly;
    #[async_trait]
    impl ParseProvider for RemoteOnly {
        async fn parse(&self, request: &ParseRequest) -> Result<ReaderOutput, ProviderError> {
            assert!(!request.remote);
            Err(ProviderError::Unavailable)
        }
    }
    // The engine being down pauses the run; it is not an error and it does not
    // throw away the pages already read.
    let paused = parse_document(
        &store,
        &doc.doc_id,
        &RemoteOnly,
        &RoutingTable::default(),
        ParseOptions {
            force: true,
            ..ParseOptions::default()
        },
    )
    .await
    .unwrap();
    assert!(paused.paused.is_some_and(|reason| reason.contains("not running")));
    let pages = store.list_pages(&doc.sha256).await.unwrap();
    assert!(
        pages.iter().all(|p| p.status != "failed"),
        "a page the engine never saw must not be marked failed; got {pages:?}"
    );
}

/// Regression test: a reader failure that is not `Unavailable`/`CloudDisabled`
/// (so the document keeps going instead of aborting) must survive the
/// unconditional `clear_issues` call made right after the reader loop. It
/// previously did not: the issue was inserted, then wiped a moment later,
/// because `page_issues` only looks at successful reader output and never
/// resurfaces a failed reader's own error.
#[tokio::test]
async fn a_non_fatal_reader_failure_is_not_erased_right_after_being_recorded() {
    let store = store().await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("note.txt");
    std::fs::write(&path, "TSN 29189").unwrap();
    let doc = add_file(&store, "proj-a", &path, IngestLimits::desktop())
        .await
        .unwrap();

    struct FlakyProvider;
    #[async_trait]
    impl ParseProvider for FlakyProvider {
        async fn parse(&self, _request: &ParseRequest) -> Result<ReaderOutput, ProviderError> {
            Err(ProviderError::Transport("mineru returned a malformed response".into()))
        }
    }

    parse_document(
        &store,
        &doc.doc_id,
        &FlakyProvider,
        &RoutingTable::default(),
        ParseOptions::default(),
    )
    .await
    .unwrap();

    let issues = store.list_issues(&doc.sha256).await.unwrap();
    assert!(
        issues
            .iter()
            .any(|i| i.code == "render_failed" && i.detail.contains("malformed response")),
        "the reader's own failure detail must still be present after clear_issues; got {issues:?}"
    );
}

#[tokio::test]
async fn search_stays_inside_the_project() {
    let store = store().await;
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.txt");
    let b = dir.path().join("b.txt");
    std::fs::write(&a, "blade RGF34904").unwrap();
    std::fs::write(&b, "blade RGF34904 resume").unwrap();
    let da = add_file(&store, "proj-a", &a, IngestLimits::desktop()).await.unwrap();
    let db = add_file(&store, "proj-b", &b, IngestLimits::desktop()).await.unwrap();
    store
        .replace_blocks(
            &da.sha256,
            1,
            &[aionui_documents::BlockRow {
                block_id: "ba".into(),
                sha256: da.sha256.clone(),
                page_no: 1,
                label: None,
                bbox: None,
                text: "blade RGF34904".into(),
                html: None,
                confidence: Some(1.0),
                reading_order: Some(0),
                provider: "test".into(),
            }],
        )
        .await
        .unwrap();
    store
        .replace_blocks(
            &db.sha256,
            1,
            &[aionui_documents::BlockRow {
                block_id: "bb".into(),
                sha256: db.sha256.clone(),
                page_no: 1,
                label: None,
                bbox: None,
                text: "personal resume".into(),
                html: None,
                confidence: Some(1.0),
                reading_order: Some(0),
                provider: "test".into(),
            }],
        )
        .await
        .unwrap();
    let hits = search_project(&store, "proj-a", "resume").await.unwrap();
    assert!(hits.is_empty());
    let hits = search_project(&store, "proj-a", "RGF34904").await.unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].project_id, "proj-a");
}

#[tokio::test]
async fn human_annotation_feeds_the_review_package() {
    let store = store().await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("life.txt");
    std::fs::write(&path, "x").unwrap();
    let doc = add_file(&store, "proj-a", &path, IngestLimits::desktop())
        .await
        .unwrap();
    annotate(
        &store,
        AnnotateRequest {
            project_id: "proj-a".into(),
            sha256: doc.sha256,
            page_no: 1,
            bbox: Some([0.1, 0.1, 0.2, 0.2]),
            metric: "csn".into(),
            raw: "4585".into(),
            value: Some(4585.0),
        },
    )
    .await
    .unwrap();
    let pkg = package_from_store(&store, "proj-a", Default::default()).await.unwrap();
    assert_eq!(pkg.observations.len(), 1);
    assert_eq!(pkg.observations[0].source, "human");
}

#[test]
fn golden_seed_waits_for_the_user() {
    let seed = seed_41744();
    assert!(seed.iter().all(|f| !f.confirmed));
    let actual = aionui_documents::FieldReading {
        metric: "tsn".into(),
        raw: "29,189:16".into(),
        value: None,
        bbox: None,
        confidence: 1.0,
        provider: "old".into(),
    };
    assert!(!field_exact_match(&seed[0], &actual));
    assert!(replay(&seed, &[actual]).unwrap().is_empty());
}

#[allow(dead_code)]
fn _keep_write_used(dir: &std::path::Path) {
    let mut f = std::fs::File::create(dir.join("x")).unwrap();
    f.write_all(b"x").unwrap();
}
