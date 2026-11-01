//! File scope for the desktop assistant.
//!
//! When a turn sets a scope, Read, Write, Edit, Grep, Glob, and ViewImage may
//! only touch the workspace and the files attached to that turn. ExecCommand
//! is refused entirely, because a shell can leave the folder with `cd`.
//! When no scope is set, the tools keep their original behavior.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

tokio::task_local! {
    static TOOL_SCOPE: Vec<PathBuf>;
}

static SCOPE_REQUIRED: AtomicBool = AtomicBool::new(false);

/// The desktop process calls this once. After that, a tool running outside a
/// turn's scope is refused instead of falling open.
pub fn require_scope() {
    SCOPE_REQUIRED.store(true, Ordering::Relaxed);
}

pub async fn with_roots<F: Future>(roots: Vec<PathBuf>, fut: F) -> F::Output {
    TOOL_SCOPE.scope(roots, fut).await
}

fn scope_required() -> bool {
    SCOPE_REQUIRED.load(Ordering::Relaxed)
}

pub fn scope_is_active() -> bool {
    TOOL_SCOPE.try_with(|_| ()).is_ok() || scope_required()
}

/// `Ok(())` when the path is inside the turn's scope, or when no scope is set.
pub fn allow_path(path: &Path) -> Result<(), String> {
    let roots = match TOOL_SCOPE.try_with(|roots| roots.clone()) {
        Ok(roots) => roots,
        Err(_) if scope_required() => {
            return Err(
                "This conversation has no project files. Refusing to read the rest of the computer.".to_string(),
            );
        }
        Err(_) => return Ok(()),
    };
    if roots.is_empty() {
        return Err("This conversation has no project files. Refusing to read the rest of the computer.".to_string());
    }
    let Some(candidate) = normalize(path) else {
        return Err(outside(path));
    };
    if roots
        .iter()
        .any(|root| normalize(root).is_some_and(|root| candidate.starts_with(&root)))
    {
        return Ok(());
    }
    Err(outside(path))
}

/// Absolute glob patterns are checked against the directory they start from.
pub fn allow_glob(pattern: &str, root: &Path) -> Result<(), String> {
    if let Some(anchor) = absolute_glob_anchor(pattern) {
        return allow_path(Path::new(anchor));
    }
    allow_path(root)
}

fn outside(path: &Path) -> String {
    format!(
        "Refusing to touch a path outside this project's files: {}",
        path.display()
    )
}

fn absolute_glob_anchor(pattern: &str) -> Option<&str> {
    if !pattern.starts_with('/') {
        return None;
    }
    let cut = pattern.find(['*', '?', '[']).unwrap_or(pattern.len());
    let prefix = pattern[..cut].trim_end_matches('/');
    if prefix.is_empty() { None } else { Some(prefix) }
}

fn normalize(path: &Path) -> Option<PathBuf> {
    if let Ok(canonical) = std::fs::canonicalize(path) {
        return Some(canonical);
    }
    let parent = path.parent().filter(|parent| !parent.as_os_str().is_empty())?;
    let name = path.file_name()?;
    Some(std::fs::canonicalize(parent).ok()?.join(name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    use crate::Tool;
    use crate::exec_command::ExecCommandTool;
    use serde_json::json;

    #[tokio::test]
    async fn unscoped_calls_are_unchanged() {
        assert!(allow_path(Path::new("/etc/passwd")).is_ok());
        assert!(!scope_is_active());
    }

    #[tokio::test]
    async fn scope_allows_the_workspace_and_rejects_the_home_folder() {
        let root = tempfile::tempdir().unwrap();
        let inside = root.path().join("blade.pdf");
        fs::write(&inside, b"x").unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();

        with_roots(vec![root.path().to_path_buf()], async {
            assert!(allow_path(&inside).is_ok());
            assert!(allow_path(outside.path()).is_err());
            assert!(allow_glob("**/*.pdf", root.path()).is_ok());
            assert!(allow_glob("/Users/**/*.pdf", root.path()).is_err());
        })
        .await;
    }

    #[tokio::test]
    async fn an_attached_file_does_not_open_its_folder() {
        let dir = tempfile::tempdir().unwrap();
        let attached = dir.path().join("41744.pdf");
        let sibling = dir.path().join("resume.pdf");
        fs::write(&attached, b"a").unwrap();
        fs::write(&sibling, b"b").unwrap();

        with_roots(vec![attached.clone()], async {
            assert!(allow_path(&attached).is_ok());
            assert!(allow_path(&sibling).is_err());
        })
        .await;
    }

    #[tokio::test]
    async fn empty_scope_refuses_every_path() {
        with_roots(Vec::new(), async {
            assert!(allow_path(Path::new("/tmp")).is_err());
        })
        .await;
    }

    #[tokio::test]
    async fn scoped_shell_is_refused() {
        let tool = ExecCommandTool::new(std::env::temp_dir());
        let result = with_roots(vec![std::env::temp_dir()], async {
            tool.execute(json!({"cmd": "pwd"})).await
        })
        .await;
        assert!(result.is_error);
        assert!(result.content.contains("ExecCommand"));
    }
}
