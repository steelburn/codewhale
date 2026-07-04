//! Structural code search and preview-first edits for a small language set.

use std::path::Path;

use async_trait::async_trait;
use serde::Serialize;
use serde_json::{Value, json};

use super::diff_format::make_unified_diff;
use super::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec,
    optional_bool, optional_str, required_str,
};

pub struct StructuralCodeTool;

#[async_trait]
impl ToolSpec for StructuralCodeTool {
    fn name(&self) -> &'static str {
        "structural_code"
    }

    fn description(&self) -> &'static str {
        "Search or edit Rust and TypeScript/JavaScript code by syntax-level symbol blocks. Supports operation=search, summary, or edit. Edits return a unified diff preview by default and only write when apply=true after the file has been read."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "operation": {
                    "type": "string",
                    "enum": ["search", "summary", "edit"],
                    "description": "Structural operation to run."
                },
                "path": {
                    "type": "string",
                    "description": "Workspace-relative source file path."
                },
                "query": {
                    "type": "string",
                    "description": "Case-sensitive substring matched against symbol name or kind. Required for search/edit."
                },
                "kind": {
                    "type": "string",
                    "description": "Optional symbol kind filter such as function, struct, class, interface, const_arrow, or impl."
                },
                "replacement": {
                    "type": "string",
                    "description": "Replacement source for operation=edit. Replaces the matched symbol block."
                },
                "apply": {
                    "type": "boolean",
                    "description": "For operation=edit only: write the previewed edit. Defaults to false."
                }
            },
            "required": ["operation", "path"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![
            ToolCapability::WritesFiles,
            ToolCapability::RequiresApproval,
            ToolCapability::Sandboxable,
        ]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Suggest
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let operation = required_str(&input, "operation")?;
        let path_str = required_str(&input, "path")?;
        let file_path = context.resolve_path(path_str)?;
        let source = std::fs::read_to_string(&file_path).map_err(|err| {
            ToolError::execution_failed(format!("failed to read {}: {err}", file_path.display()))
        })?;
        let language = language_for_path(&file_path)?;
        let symbols = parse_symbols(&source, language)?;

        match operation {
            "summary" => Ok(ToolResult::success(render_summary(&file_path, &symbols))),
            "search" => {
                let query = required_str(&input, "query")?;
                let matches = filter_symbols(&symbols, query, optional_str(&input, "kind"));
                Ok(ToolResult::success(format_json(&json!({
                    "path": path_str,
                    "language": language.as_str(),
                    "matches": matches,
                    "fallback": "Use grep_files/read_file/edit_file for unsupported structural patterns."
                }))))
            }
            "edit" => {
                let query = required_str(&input, "query")?;
                let replacement = required_str(&input, "replacement")?;
                let matches = filter_symbols(&symbols, query, optional_str(&input, "kind"));
                if matches.is_empty() {
                    return Err(ToolError::execution_failed(
                        "structural edit matched no symbols; use structural_code search or grep_files to inspect candidates",
                    ));
                }
                if matches.len() > 1 {
                    return Err(ToolError::execution_failed(format!(
                        "structural edit matched {} symbols; narrow query or kind before applying",
                        matches.len()
                    )));
                }
                let selected = matches[0];
                let mut updated = source.clone();
                updated.replace_range(selected.start_byte..selected.end_byte, replacement);
                let diff = make_unified_diff(&file_path.display().to_string(), &source, &updated);
                let apply = optional_bool(&input, "apply", false);
                if apply {
                    context.require_fresh_file_read(&file_path, path_str)?;
                    crate::utils::write_atomic(&file_path, updated.as_bytes()).map_err(|err| {
                        ToolError::execution_failed(format!(
                            "failed to write {}: {err}",
                            file_path.display()
                        ))
                    })?;
                    context.note_file_read(&file_path);
                }
                let summary = if apply {
                    format!(
                        "Applied structural edit to {} `{}`.",
                        selected.kind, selected.name
                    )
                } else {
                    "Preview only. Re-run with apply=true after reading the file to write this structural edit.".to_string()
                };
                Ok(ToolResult::success(format!("{diff}\n{summary}")))
            }
            other => Err(ToolError::invalid_input(format!(
                "unknown structural_code operation `{other}`"
            ))),
        }
    }
}

pub(crate) fn summarize_file(path: &Path, source: &str) -> Result<String, ToolError> {
    let language = language_for_path(path)?;
    let symbols = parse_symbols(source, language)?;
    Ok(render_summary(path, &symbols))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Language {
    Rust,
    TypeScript,
}

impl Language {
    fn as_str(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::TypeScript => "typescript",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
struct Symbol<'a> {
    kind: &'a str,
    name: &'a str,
    start_line: usize,
    end_line: usize,
    #[serde(skip)]
    start_byte: usize,
    #[serde(skip)]
    end_byte: usize,
}

fn language_for_path(path: &Path) -> Result<Language, ToolError> {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("rs") => Ok(Language::Rust),
        Some("ts" | "tsx" | "js" | "jsx") => Ok(Language::TypeScript),
        _ => Err(ToolError::execution_failed(
            "unsupported language for structural_code; supported: Rust (.rs), TypeScript/JavaScript (.ts/.tsx/.js/.jsx). Fallback: use grep_files/read_file/edit_file.",
        )),
    }
}

fn parse_symbols(source: &str, language: Language) -> Result<Vec<Symbol<'_>>, ToolError> {
    let mut symbols = Vec::new();
    let mut offset = 0usize;
    for (line_idx, line) in source.split_inclusive('\n').enumerate() {
        let line_without_nl = line.strip_suffix('\n').unwrap_or(line);
        if let Some((kind, name, brace_relative)) = detect_symbol(line_without_nl, language) {
            let start = offset;
            let end = if let Some(brace_relative) = brace_relative {
                let brace = offset + brace_relative;
                find_matching_brace(source, brace)?
            } else {
                offset + line_without_nl.len()
            };
            symbols.push(Symbol {
                kind,
                name,
                start_line: line_idx + 1,
                end_line: line_for_offset(source, end),
                start_byte: start,
                end_byte: end,
            });
        }
        offset += line.len();
    }
    Ok(symbols)
}

fn detect_symbol<'a>(
    line: &'a str,
    language: Language,
) -> Option<(&'static str, &'a str, Option<usize>)> {
    match language {
        Language::Rust => detect_rust_symbol(line),
        Language::TypeScript => detect_ts_symbol(line),
    }
}

fn detect_rust_symbol(line: &str) -> Option<(&'static str, &str, Option<usize>)> {
    let trimmed = line.trim_start();
    let without_vis = trimmed.strip_prefix("pub ").unwrap_or(trimmed);
    let without_async = without_vis.strip_prefix("async ").unwrap_or(without_vis);
    for (keyword, kind) in [
        ("fn ", "function"),
        ("struct ", "struct"),
        ("enum ", "enum"),
        ("trait ", "trait"),
        ("impl ", "impl"),
    ] {
        if let Some(rest) = without_async.strip_prefix(keyword) {
            let name = symbol_name(rest)?;
            return Some((kind, name, line.find('{')));
        }
    }
    None
}

fn detect_ts_symbol(line: &str) -> Option<(&'static str, &str, Option<usize>)> {
    let trimmed = line.trim_start();
    let without_export = trimmed.strip_prefix("export ").unwrap_or(trimmed);
    for (keyword, kind) in [
        ("async function ", "function"),
        ("function ", "function"),
        ("class ", "class"),
        ("interface ", "interface"),
    ] {
        if let Some(rest) = without_export.strip_prefix(keyword) {
            let name = symbol_name(rest)?;
            return Some((kind, name, line.find('{')));
        }
    }
    if let Some(rest) = without_export.strip_prefix("const ")
        && let Some((name, rhs)) = rest.split_once('=')
        && rhs.contains("=>")
    {
        let name = symbol_name(name.trim())?;
        return Some(("const_arrow", name, line.find('{')));
    }
    None
}

fn symbol_name(rest: &str) -> Option<&str> {
    let end = rest
        .find(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
        .unwrap_or(rest.len());
    (end > 0).then_some(&rest[..end])
}

fn find_matching_brace(source: &str, open: usize) -> Result<usize, ToolError> {
    let mut depth = 0usize;
    for (idx, ch) in source[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Ok(open + idx + ch.len_utf8());
                }
            }
            _ => {}
        }
    }
    Err(ToolError::execution_failed(
        "parse error: unmatched `{` while scanning structural symbol block",
    ))
}

fn line_for_offset(source: &str, offset: usize) -> usize {
    source[..offset.min(source.len())]
        .bytes()
        .filter(|b| *b == b'\n')
        .count()
        + 1
}

fn filter_symbols<'a>(
    symbols: &'a [Symbol<'a>],
    query: &str,
    kind: Option<&str>,
) -> Vec<&'a Symbol<'a>> {
    symbols
        .iter()
        .filter(|symbol| kind.is_none_or(|wanted| symbol.kind == wanted))
        .filter(|symbol| {
            query.is_empty() || symbol.name.contains(query) || symbol.kind.contains(query)
        })
        .collect()
}

fn render_summary(path: &Path, symbols: &[Symbol<'_>]) -> String {
    let mut out = format!(
        "<structure path=\"{}\" symbols=\"{}\">\n",
        path.display(),
        symbols.len()
    );
    for symbol in symbols {
        out.push_str(&format!(
            "- {} `{}` lines {}-{}\n",
            symbol.kind, symbol.name, symbol.start_line, symbol.end_line
        ));
    }
    out.push_str("</structure>");
    out
}

fn format_json(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn structural_search_finds_rust_function() {
        let source = "pub fn whale() {\n    println!(\"hi\");\n}\n";
        let symbols = parse_symbols(source, Language::Rust).unwrap();
        let matches = filter_symbols(&symbols, "whale", Some("function"));
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].start_line, 1);
        assert_eq!(matches[0].end_line, 3);
    }

    #[test]
    fn structural_search_finds_typescript_class() {
        let source = "export class Whale {\n  swim() {}\n}\n";
        let symbols = parse_symbols(source, Language::TypeScript).unwrap();
        let matches = filter_symbols(&symbols, "Whale", Some("class"));
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].kind, "class");
    }

    #[test]
    fn structural_parse_error_reports_unmatched_brace() {
        let err = parse_symbols("fn broken() {\n", Language::Rust).unwrap_err();
        assert!(err.to_string().contains("unmatched"), "{err}");
    }

    #[test]
    fn unsupported_language_has_fallback_guidance() {
        let err = language_for_path(Path::new("notes.md")).unwrap_err();
        assert!(err.to_string().contains("grep_files"), "{err}");
    }

    #[tokio::test]
    async fn edit_previews_without_writing() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("main.rs");
        std::fs::write(&path, "fn old() {\n    old_call();\n}\n").unwrap();
        let ctx = ToolContext::new(tmp.path());
        let result = StructuralCodeTool
            .execute(
                json!({
                    "operation": "edit",
                    "path": "main.rs",
                    "query": "old",
                    "kind": "function",
                    "replacement": "fn new() {\n    new_call();\n}\n"
                }),
                &ctx,
            )
            .await
            .unwrap();
        assert!(result.content.contains("Preview only"));
        assert!(result.content.contains("+fn new()"));
        assert!(std::fs::read_to_string(&path).unwrap().contains("old_call"));
    }

    #[tokio::test]
    async fn edit_apply_requires_fresh_read() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("main.rs");
        std::fs::write(&path, "fn old() {}\n").unwrap();
        let ctx = ToolContext::new(tmp.path());
        let err = StructuralCodeTool
            .execute(
                json!({
                    "operation": "edit",
                    "path": "main.rs",
                    "query": "old",
                    "replacement": "fn new() {}\n",
                    "apply": true
                }),
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("read_file"), "{err}");
    }
}
