use std::io::Write;

use aionui_db::init_database_memory;
use aionui_documents::{
    AnnotateRequest, DocumentStore, IngestLimits, RoutingTable, add_file, annotate, field_exact_match, package_from_store,
    parse_document, replay, search_project, seed_41744,
};
use aionui_documents::{
    FsChange, ParseProvider, ParseRequest, ProviderError, ReaderOutput, apply_fs_event,
};
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

    let added = add_file(&store, "proj-a", &path, IngestLimits::desktop()).await.unwrap();
    assert_eq!(added.page_count, 1);
    let same = add_file(&store, "proj-a", &path, IngestLimits::desktop()).await.unwrap();
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
    let edited = add_file(&store, "proj-a", &renamed, IngestLimits::desktop()).await.unwrap();
    assert_ne!(edited.sha256, added.sha256);
    assert_eq!(
        store.find_by_id(&added.doc_id).await.unwrap().unwrap().superseded_by.as_deref(),
        Some(edited.doc_id.as_str())
    );

    apply_fs_event(&store, "proj-a", FsChange::Delete(renamed), IngestLimits::desktop())
        .await
        .unwrap();
    assert!(store.find_by_id(&edited.doc_id).await.unwrap().unwrap().deleted_at.is_some());
}

#[tokio::test]
async fn page_quota_rejects_oversize() {
    let store = store().await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("big.txt");
    std::fs::write(&path, "x").unwrap();
    let err = add_file(&store, "p", &path, IngestLimits { max_pages: 0 }).await.unwrap_err();
    assert!(err.to_string().contains("limit"));
}

#[tokio::test]
async fn dual_readers_and_local_only() {
    let store = store().await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("note.txt");
    std::fs::write(&path, "TSN 29189").unwrap();
    let doc = add_file(&store, "proj-a", &path, IngestLimits::desktop()).await.unwrap();
    let provider = FakeProvider {
        text: "TSN 29189".into(),
    };
    let report = parse_document(&store, &doc.doc_id, &provider, &RoutingTable::default())
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
    let denied = parse_document(&store, &doc.doc_id, &RemoteOnly, &RoutingTable::default()).await;
    assert!(denied.unwrap_err().contains("not running"));
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
    let doc = add_file(&store, "proj-a", &path, IngestLimits::desktop()).await.unwrap();

    struct FlakyProvider;
    #[async_trait]
    impl ParseProvider for FlakyProvider {
        async fn parse(&self, _request: &ParseRequest) -> Result<ReaderOutput, ProviderError> {
            Err(ProviderError::Transport("mineru returned a malformed response".into()))
        }
    }

    parse_document(&store, &doc.doc_id, &FlakyProvider, &RoutingTable::default())
        .await
        .unwrap();

    let issues = store.list_issues(&doc.sha256).await.unwrap();
    assert!(
        issues.iter().any(|i| i.code == "render_failed" && i.detail.contains("malformed response")),
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
    let doc = add_file(&store, "proj-a", &path, IngestLimits::desktop()).await.unwrap();
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
