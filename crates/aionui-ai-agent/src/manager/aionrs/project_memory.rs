use std::path::Path;

use aion_types::message::ContentBlock;
use aionui_common::Confirmation;
use serde::Deserialize;

const MEMORY_REL: &str = ".aionui/workbench-memory.json";
const PACKAGE_REL: &str = "records-review.json";
const MAX_CHARS: usize = 4000;

#[derive(Debug, Clone, Default, Deserialize)]
struct WorkbenchMemoryFile {
    #[serde(default)]
    last_show: Option<serde_json::Value>,
    #[serde(default)]
    last_lookup: Option<serde_json::Value>,
    #[serde(default)]
    last_conclusion: Option<String>,
}

/// Insert a short project-memory block in front of the user turn when this
/// workspace looks like an engineer project.
pub fn prepend_project_memory(
    blocks: Vec<ContentBlock>,
    workspace: &str,
    conversation_id: &str,
    pending: &[Confirmation],
) -> Vec<ContentBlock> {
    match project_memory_block(workspace, conversation_id, pending) {
        Some(text) => {
            let mut out = Vec::with_capacity(blocks.len() + 1);
            out.push(ContentBlock::Text { text });
            out.extend(blocks);
            out
        }
        None => blocks,
    }
}

pub fn project_memory_block(workspace: &str, conversation_id: &str, pending: &[Confirmation]) -> Option<String> {
    if workspace.trim().is_empty() {
        return None;
    }
    let root = Path::new(workspace);
    let memory = read_memory(root);
    if memory.is_none() && !root.join(PACKAGE_REL).is_file() {
        return None;
    }
    let memory = memory.unwrap_or_default();
    let project = root.file_name().and_then(|name| name.to_str()).unwrap_or(workspace);
    let mut lines = vec![
        "[Project memory]".to_string(),
        format!("Project: {project}"),
        format!("Conversation: {conversation_id}"),
    ];
    if let Some(show) = memory.last_show.as_ref() {
        lines.push(format!("Last opened: {}", compact_json(show)));
    }
    if let Some(lookup) = memory.last_lookup.as_ref() {
        lines.push(format!("Last card: {}", compact_json(lookup)));
    }
    if let Some(conclusion) = memory
        .last_conclusion
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        lines.push(format!("Last conclusion: {conclusion}"));
    }
    if !pending.is_empty() {
        let titles: Vec<&str> = pending
            .iter()
            .map(|item| {
                item.title
                    .as_deref()
                    .filter(|value| !value.is_empty())
                    .unwrap_or(item.description.as_str())
            })
            .take(5)
            .collect();
        lines.push(format!("Pending confirmations: {}", titles.join("; ")));
    }
    lines.push("Facts are not stored here. Call workbench_lookup when numbers are needed, then workbench_show.".into());
    Some(truncate_chars(&lines.join("\n"), MAX_CHARS))
}

fn read_memory(root: &Path) -> Option<WorkbenchMemoryFile> {
    let raw = std::fs::read_to_string(root.join(MEMORY_REL)).ok()?;
    serde_json::from_str(&raw).ok()
}

fn compact_json(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Object(map) => {
            let parts: Vec<String> = map
                .iter()
                .filter_map(|(key, item)| {
                    let text = match item {
                        serde_json::Value::Null => return None,
                        serde_json::Value::String(value) if value.is_empty() => return None,
                        serde_json::Value::String(value) => value.clone(),
                        other => other.to_string(),
                    };
                    Some(format!("{key}={text}"))
                })
                .take(8)
                .collect();
            parts.join(" ")
        }
        other => other.to_string(),
    }
}

fn truncate_chars(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        return text.to_string();
    }
    text.chars().take(max.saturating_sub(1)).collect::<String>() + "…"
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn skips_non_project_workspaces() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(project_memory_block(dir.path().to_str().unwrap(), "c1", &[]), None);
    }

    #[test]
    fn injects_last_show_and_pending_when_package_exists() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(PACKAGE_REL), "{}").unwrap();
        fs::create_dir_all(dir.path().join(".aionui")).unwrap();
        fs::write(
            dir.path().join(MEMORY_REL),
            r#"{"last_show":{"page":"llp","part":"RGF34904","focus":"life"},"last_conclusion":"9751 cycles left"}"#,
        )
        .unwrap();
        let pending = [Confirmation {
            id: "1".into(),
            call_id: "1".into(),
            title: Some("Allow write".into()),
            action: None,
            description: "write".into(),
            command_type: None,
            options: vec![],
            questions: None,
        }];
        let text = project_memory_block(dir.path().to_str().unwrap(), "conv-1", &pending).unwrap();
        assert!(text.contains("Conversation: conv-1"));
        assert!(text.contains("part=RGF34904"));
        assert!(text.contains("9751 cycles left"));
        assert!(text.contains("Allow write"));
        assert!(text.chars().count() <= MAX_CHARS);
    }

    #[test]
    fn prepends_a_leading_text_block() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(PACKAGE_REL), "{}").unwrap();
        let blocks = prepend_project_memory(
            vec![ContentBlock::Text {
                text: "user question".into(),
            }],
            dir.path().to_str().unwrap(),
            "c1",
            &[],
        );
        assert_eq!(blocks.len(), 2);
        match &blocks[0] {
            ContentBlock::Text { text } => assert!(text.starts_with("[Project memory]")),
            other => panic!("unexpected block: {other:?}"),
        }
    }
}
