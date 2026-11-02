//! Drains the page queue one page at a time.
//!
//! The vision model parses one page at a time, so a single worker is the whole
//! of the concurrency. A parse request only enqueues and returns; this loop is
//! what moves the queue, and it stops the moment the engine goes away so a page
//! is handed back instead of marked failed.

use std::sync::atomic::Ordering;
use std::time::Duration;

use crate::cascade::{PageParseOutcome, parse_page};
use crate::provider::ParseProvider;
use crate::queue::ParseTask;
use crate::routes::DocumentState;

const OWNER: &str = "aionui";

pub async fn run_queue<P: ParseProvider>(state: &DocumentState, provider: &P) {
    // There is one worker, so any page still leased to it was left by a run the
    // app did not finish. Without this it would wait out the full lease.
    let _ = state.queue.release_owner(OWNER).await;
    loop {
        let Some(task) = next_task(state).await else {
            break;
        };
        if !settle(state, provider, &task).await {
            break;
        }
    }
    state.worker_running.store(false, Ordering::SeqCst);
}

/// One claim, and a second after a short wait. A request can enqueue in the
/// moment between the queue looking empty and this worker exiting; the second
/// look catches it. Anything that still lands in that window stays queued and
/// is picked up by the next parse request.
async fn next_task(state: &DocumentState) -> Option<ParseTask> {
    for attempt in 0..2 {
        if let Some(task) = state
            .queue
            .claim(OWNER, 1)
            .await
            .ok()
            .and_then(|tasks| tasks.into_iter().next())
        {
            return Some(task);
        }
        if attempt == 0 {
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    }
    None
}

/// `false` means the engine is down and the loop should stop.
async fn settle<P: ParseProvider>(state: &DocumentState, provider: &P, task: &ParseTask) -> bool {
    match parse_page(&state.store, &task.doc_id, task.page_no, provider, &state.routing).await {
        Ok(PageParseOutcome::Done) => {
            let _ = state.queue.complete(&task.task_id).await;
            true
        }
        Ok(PageParseOutcome::Paused(reason)) => {
            let _ = state.queue.release(&task.task_id).await;
            *state.pause_reason.lock().await = Some(reason);
            false
        }
        Err(msg) => {
            let _ = state.queue.fail(&task.task_id, &msg).await;
            true
        }
    }
}
