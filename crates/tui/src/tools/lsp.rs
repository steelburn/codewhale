//! Model-facing LSP tool: navigation, references, code actions, and rename.

use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};

use super::diff_format::make_unified_diff;
use super::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec,
    optional_bool, optional_str, required_str,
};

pub struct LspTool;

#[async_trait]
impl ToolSpec for LspTool {
    fn name(&self) -> &'static str {
        "lsp"
    }

    fn description(&self) -> &'static str {
        "Ask the active language server for code intelligence. Supports definition, references, rename, code_action, and raw_request. Rename returns a diff preview by default and only writes when apply=true; writes require the same fresh read gate as edit_file."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "operation": {
                    "type": "string",
                    "enum": ["definition", "references", "rename", "code_action", "raw_request"],
                    "description": "LSP operation to run."
                },
                "path": {
                    "type": "string",
                    "description": "Workspace-relative file path."
                },
                "line": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "1-based line for position-based requests."
                },
                "character": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "1-based character offset for position-based requests. ASCII fixtures map exactly."
                },
                "new_name": {
                    "type": "string",
                    "description": "New symbol name for rename."
                },
                "apply": {
                    "type": "boolean",
                    "description": "For rename only: apply the returned workspace edit after previewing it. Defaults to false."
                },
                "raw_method": {
                    "type": "string",
                    "description": "For raw_request: LSP method string."
                },
                "raw_params": {
                    "type": "object",
                    "description": "For raw_request: JSON params to send."
                }
            },
            "required": ["operation", "path"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![
            ToolCapability::WritesFiles,
            ToolCapability::RequiresApproval,
        ]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Suggest
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let operation = required_str(&input, "operation")?;
        let path_str = required_str(&input, "path")?;
        let file_path = context.resolve_path(path_str)?;
        let manager = context.lsp_manager.as_ref().ok_or_else(|| {
            ToolError::execution_failed(
                "LSP is not configured for this session; enable [lsp] and retry.",
            )
        })?;

        let result = match operation {
            "definition" => {
                let params = position_params(&file_path, &input)?;
                manager
                    .request_for_file(&file_path, "textDocument/definition", params)
                    .await
            }
            "references" => {
                let mut params = position_params(&file_path, &input)?;
                params["context"] = json!({ "includeDeclaration": true });
                manager
                    .request_for_file(&file_path, "textDocument/references", params)
                    .await
            }
            "code_action" => {
                let params = code_action_params(&file_path, &input)?;
                manager
                    .request_for_file(&file_path, "textDocument/codeAction", params)
                    .await
            }
            "raw_request" => {
                let method = optional_str(&input, "raw_method")
                    .ok_or_else(|| ToolError::invalid_input("raw_request requires raw_method"))?;
                let params = input
                    .get("raw_params")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                manager.request_for_file(&file_path, method, params).await
            }
            "rename" => {
                let new_name = optional_str(&input, "new_name")
                    .ok_or_else(|| ToolError::invalid_input("rename requires new_name"))?;
                let mut params = position_params(&file_path, &input)?;
                params["newName"] = Value::String(new_name.to_string());
                let edit = manager
                    .request_for_file(&file_path, "textDocument/rename", params)
                    .await
                    .map_err(|err| ToolError::execution_failed(err.to_string()))?;
                return handle_rename_workspace_edit(
                    edit,
                    context,
                    optional_bool(&input, "apply", false),
                )
                .await;
            }
            other => {
                return Err(ToolError::invalid_input(format!(
                    "unknown lsp operation `{other}`"
                )));
            }
        }
        .map_err(|err| ToolError::execution_failed(err.to_string()))?;

        Ok(ToolResult::success(format_json(&json!({
            "operation": operation,
            "path": path_str,
            "result": result,
        }))))
    }
}

fn position_params(file_path: &Path, input: &Value) -> Result<Value, ToolError> {
    let line = required_u64_like(input, "line")?;
    let character = required_u64_like(input, "character")?;
    Ok(json!({
        "textDocument": { "uri": crate::lsp::client::uri_from_path(file_path) },
        "position": {
            "line": line.saturating_sub(1),
            "character": character.saturating_sub(1),
        }
    }))
}

fn code_action_params(file_path: &Path, input: &Value) -> Result<Value, ToolError> {
    let line = required_u64_like(input, "line")?.saturating_sub(1);
    let character = required_u64_like(input, "character")?.saturating_sub(1);
    Ok(json!({
        "textDocument": { "uri": crate::lsp::client::uri_from_path(file_path) },
        "range": {
            "start": { "line": line, "character": character },
            "end": { "line": line, "character": character },
        },
        "context": { "diagnostics": [] }
    }))
}

fn required_u64_like(input: &Value, key: &str) -> Result<u64, ToolError> {
    input
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| ToolError::invalid_input(format!("missing integer field `{key}`")))
}

async fn handle_rename_workspace_edit(
    edit: Value,
    context: &ToolContext,
    apply: bool,
) -> Result<ToolResult, ToolError> {
    let changes = parse_workspace_changes(&edit)?;
    if changes.is_empty() {
        return Ok(ToolResult::success("rename returned no workspace edits"));
    }

    let mut previews = Vec::new();
    let mut applied = Vec::new();
    for change in changes {
        let old = tokio::fs::read_to_string(&change.path)
            .await
            .map_err(|err| {
                ToolError::execution_failed(format!(
                    "failed to read {}: {err}",
                    change.path.display()
                ))
            })?;
        let new = apply_text_edits(&old, &change.edits)?;
        let diff = make_unified_diff(&change.path.display().to_string(), &old, &new);
        if !diff.is_empty() {
            previews.push(diff);
        }

        if apply {
            context.require_fresh_file_read(&change.path, &change.path.display().to_string())?;
            crate::utils::write_atomic(&change.path, new.as_bytes()).map_err(|err| {
                ToolError::execution_failed(format!(
                    "failed to write {}: {err}",
                    change.path.display()
                ))
            })?;
            context.note_file_read(&change.path);
            applied.push(change.path.display().to_string());
        }
    }

    let preview = if previews.is_empty() {
        "(rename produced no textual diff)".to_string()
    } else {
        previews.join("\n")
    };
    let summary = if apply {
        format!("Applied LSP rename to {} file(s).", applied.len())
    } else {
        "Preview only. Re-run with apply=true to write these edits after reading the affected files."
            .to_string()
    };
    Ok(ToolResult::success(format!("{preview}\n{summary}")))
}

#[derive(Debug)]
struct FileChange {
    path: PathBuf,
    edits: Vec<TextEdit>,
}

#[derive(Debug)]
struct TextEdit {
    start_line: u64,
    start_character: u64,
    end_line: u64,
    end_character: u64,
    new_text: String,
}

fn parse_workspace_changes(edit: &Value) -> Result<Vec<FileChange>, ToolError> {
    let Some(changes) = edit.get("changes").and_then(Value::as_object) else {
        if edit.get("documentChanges").is_some() {
            return Err(ToolError::execution_failed(
                "LSP documentChanges are not supported yet; request raw_request for diagnostics or retry with a server that returns changes.",
            ));
        }
        return Ok(Vec::new());
    };

    let mut out = Vec::new();
    for (uri, edits) in changes {
        let path = crate::lsp::client::path_from_uri(uri)
            .ok_or_else(|| ToolError::execution_failed(format!("unsupported LSP URI `{uri}`")))?;
        let raw_edits = edits
            .as_array()
            .ok_or_else(|| ToolError::execution_failed("workspace edit list is not an array"))?;
        let mut parsed = Vec::new();
        for edit in raw_edits {
            parsed.push(parse_text_edit(edit)?);
        }
        out.push(FileChange {
            path,
            edits: parsed,
        });
    }
    Ok(out)
}

fn parse_text_edit(edit: &Value) -> Result<TextEdit, ToolError> {
    let range = edit
        .get("range")
        .ok_or_else(|| ToolError::execution_failed("text edit missing range"))?;
    let start = range
        .get("start")
        .ok_or_else(|| ToolError::execution_failed("text edit missing start"))?;
    let end = range
        .get("end")
        .ok_or_else(|| ToolError::execution_failed("text edit missing end"))?;
    Ok(TextEdit {
        start_line: json_u64(start, "line")?,
        start_character: json_u64(start, "character")?,
        end_line: json_u64(end, "line")?,
        end_character: json_u64(end, "character")?,
        new_text: edit
            .get("newText")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
    })
}

fn json_u64(value: &Value, key: &str) -> Result<u64, ToolError> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| ToolError::execution_failed(format!("missing numeric `{key}`")))
}

fn apply_text_edits(old: &str, edits: &[TextEdit]) -> Result<String, ToolError> {
    let mut ranges = Vec::new();
    for edit in edits {
        let start = offset_for_position(old, edit.start_line, edit.start_character)?;
        let end = offset_for_position(old, edit.end_line, edit.end_character)?;
        if start > end {
            return Err(ToolError::execution_failed(
                "LSP text edit range is inverted",
            ));
        }
        ranges.push((start, end, edit.new_text.clone()));
    }
    ranges.sort_by(|a, b| b.0.cmp(&a.0));

    let mut next = old.to_string();
    for (start, end, replacement) in ranges {
        next.replace_range(start..end, &replacement);
    }
    Ok(next)
}

fn offset_for_position(text: &str, line: u64, character: u64) -> Result<usize, ToolError> {
    let mut offset = 0usize;
    for (idx, segment) in text.split_inclusive('\n').enumerate() {
        if idx as u64 == line {
            let line_body = segment.strip_suffix('\n').unwrap_or(segment);
            if character == 0 {
                return Ok(offset);
            }
            if let Some((byte, ch)) = line_body.char_indices().nth(character as usize - 1) {
                return Ok(offset + byte + ch.len_utf8());
            }
            if character as usize == line_body.chars().count() {
                return Ok(offset + line_body.len());
            }
            return Err(ToolError::execution_failed(
                "LSP position is past end of line",
            ));
        }
        offset += segment.len();
    }
    if line as usize == text.lines().count() && character == 0 {
        Ok(text.len())
    } else {
        Err(ToolError::execution_failed(
            "LSP position is past end of file",
        ))
    }
}

fn format_json(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lsp::{LspConfig, LspManager, LspTransport, registry::Language};
    use anyhow::Result;
    use std::sync::{Arc, Mutex};
    use tempfile::tempdir;

    struct RequestFake {
        calls: Mutex<Vec<(String, Value)>>,
        response: Value,
    }

    #[async_trait]
    impl LspTransport for RequestFake {
        async fn diagnostics_for(
            &self,
            _path: &Path,
            _text: &str,
            _wait: Duration,
        ) -> Result<Vec<crate::lsp::Diagnostic>> {
            Ok(Vec::new())
        }

        async fn request(&self, method: &str, params: Value, _wait: Duration) -> Result<Value> {
            self.calls
                .lock()
                .unwrap()
                .push((method.to_string(), params));
            Ok(self.response.clone())
        }

        async fn shutdown(&self) {}
    }

    #[tokio::test]
    async fn definition_forwards_position_request() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("main.rs");
        tokio::fs::write(&path, "fn main() {}\n").await.unwrap();
        let manager = Arc::new(LspManager::new(
            LspConfig::default(),
            tmp.path().to_path_buf(),
        ));
        let fake = Arc::new(RequestFake {
            calls: Mutex::new(Vec::new()),
            response: json!([{"uri": crate::lsp::client::uri_from_path(&path)}]),
        });
        manager
            .install_test_transport(Language::Rust, fake.clone())
            .await;
        let ctx = ToolContext::new(tmp.path()).with_lsp_manager(manager);

        let result = LspTool
            .execute(
                json!({"operation": "definition", "path": "main.rs", "line": 1, "character": 4}),
                &ctx,
            )
            .await
            .unwrap();

        assert!(result.content.contains("main.rs"));
        let calls = fake.calls.lock().unwrap();
        assert_eq!(calls[0].0, "textDocument/definition");
        assert_eq!(calls[0].1["position"]["line"], 0);
    }

    #[tokio::test]
    async fn references_request_includes_declaration_context() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("main.rs");
        tokio::fs::write(&path, "fn main() {}\n").await.unwrap();
        let manager = Arc::new(LspManager::new(
            LspConfig::default(),
            tmp.path().to_path_buf(),
        ));
        let fake = Arc::new(RequestFake {
            calls: Mutex::new(Vec::new()),
            response: json!([]),
        });
        manager
            .install_test_transport(Language::Rust, fake.clone())
            .await;
        let ctx = ToolContext::new(tmp.path()).with_lsp_manager(manager);

        LspTool
            .execute(
                json!({"operation": "references", "path": "main.rs", "line": 1, "character": 4}),
                &ctx,
            )
            .await
            .unwrap();

        let calls = fake.calls.lock().unwrap();
        assert_eq!(calls[0].0, "textDocument/references");
        assert_eq!(calls[0].1["context"]["includeDeclaration"], true);
    }

    #[tokio::test]
    async fn code_action_returns_action_metadata() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("main.rs");
        tokio::fs::write(&path, "fn main() {}\n").await.unwrap();
        let manager = Arc::new(LspManager::new(
            LspConfig::default(),
            tmp.path().to_path_buf(),
        ));
        let fake = Arc::new(RequestFake {
            calls: Mutex::new(Vec::new()),
            response: json!([{"title": "Extract function", "kind": "refactor.extract"}]),
        });
        manager
            .install_test_transport(Language::Rust, fake.clone())
            .await;
        let ctx = ToolContext::new(tmp.path()).with_lsp_manager(manager);

        let result = LspTool
            .execute(
                json!({"operation": "code_action", "path": "main.rs", "line": 1, "character": 4}),
                &ctx,
            )
            .await
            .unwrap();

        assert!(result.content.contains("Extract function"));
        assert!(result.content.contains("refactor.extract"));
        let calls = fake.calls.lock().unwrap();
        assert_eq!(calls[0].0, "textDocument/codeAction");
    }

    #[tokio::test]
    async fn rename_previews_workspace_edit_without_writing() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("main.rs");
        tokio::fs::write(&path, "fn old_name() {}\nfn main() { old_name(); }\n")
            .await
            .unwrap();
        let uri = crate::lsp::client::uri_from_path(&path);
        let manager = Arc::new(LspManager::new(
            LspConfig::default(),
            tmp.path().to_path_buf(),
        ));
        let fake = Arc::new(RequestFake {
            calls: Mutex::new(Vec::new()),
            response: json!({
                "changes": {
                    uri: [
                        {"range": {"start": {"line": 0, "character": 3}, "end": {"line": 0, "character": 11}}, "newText": "new_name"},
                        {"range": {"start": {"line": 1, "character": 12}, "end": {"line": 1, "character": 20}}, "newText": "new_name"}
                    ]
                }
            }),
        });
        manager
            .install_test_transport(Language::Rust, fake.clone())
            .await;
        let ctx = ToolContext::new(tmp.path()).with_lsp_manager(manager);

        let result = LspTool
            .execute(
                json!({"operation": "rename", "path": "main.rs", "line": 1, "character": 4, "new_name": "new_name"}),
                &ctx,
            )
            .await
            .unwrap();

        assert!(result.content.contains("Preview only"));
        assert!(result.content.contains("+fn new_name()"));
        let disk = tokio::fs::read_to_string(&path).await.unwrap();
        assert!(disk.contains("old_name"));
    }

    #[tokio::test]
    async fn rename_apply_requires_fresh_read() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("main.rs");
        tokio::fs::write(&path, "fn old_name() {}\n").await.unwrap();
        let uri = crate::lsp::client::uri_from_path(&path);
        let manager = Arc::new(LspManager::new(
            LspConfig::default(),
            tmp.path().to_path_buf(),
        ));
        let fake = Arc::new(RequestFake {
            calls: Mutex::new(Vec::new()),
            response: json!({
                "changes": {
                    uri: [
                        {"range": {"start": {"line": 0, "character": 3}, "end": {"line": 0, "character": 11}}, "newText": "new_name"}
                    ]
                }
            }),
        });
        manager
            .install_test_transport(Language::Rust, fake.clone())
            .await;
        let ctx = ToolContext::new(tmp.path()).with_lsp_manager(manager);

        let err = LspTool
            .execute(
                json!({"operation": "rename", "path": "main.rs", "line": 1, "character": 4, "new_name": "new_name", "apply": true}),
                &ctx,
            )
            .await
            .unwrap_err();

        assert!(err.to_string().contains("read_file"), "{err}");
    }

    #[test]
    fn applies_multiple_text_edits_from_bottom_up() {
        let old = "fn old_name() {}\nfn main() { old_name(); }\n";
        let edits = vec![
            TextEdit {
                start_line: 0,
                start_character: 3,
                end_line: 0,
                end_character: 11,
                new_text: "new_name".to_string(),
            },
            TextEdit {
                start_line: 1,
                start_character: 12,
                end_line: 1,
                end_character: 20,
                new_text: "new_name".to_string(),
            },
        ];
        let next = apply_text_edits(old, &edits).unwrap();
        assert_eq!(next, "fn new_name() {}\nfn main() { new_name(); }\n");
    }
}
