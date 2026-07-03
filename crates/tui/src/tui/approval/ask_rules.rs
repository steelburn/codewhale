use codewhale_config::ToolAskRule;
use serde_json::Value;
use std::path::Path;

/// Human-readable preview of ask-only rules the `S` approval shortcut would
/// append. This is intentionally derived from `persistent_ask_rules` only; the
/// approval UI must not re-parse tool inputs such as patches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskRuleSavePreview {
    pub rule_count: usize,
    pub entries: Vec<String>,
    pub omitted: usize,
}

impl AskRuleSavePreview {
    #[must_use]
    pub fn summary(&self) -> String {
        let noun = if self.rule_count == 1 {
            "rule"
        } else {
            "rules"
        };
        format!("{} ask {noun}", self.rule_count)
    }
}

const ASK_RULE_SAVE_PREVIEW_MAX_ENTRIES: usize = 4;

#[must_use]
pub(super) fn build_ask_rule_save_preview(rules: &[ToolAskRule]) -> Option<AskRuleSavePreview> {
    build_ask_rule_save_preview_with_limit(rules, ASK_RULE_SAVE_PREVIEW_MAX_ENTRIES)
}

#[must_use]
fn build_ask_rule_save_preview_with_limit(
    rules: &[ToolAskRule],
    max_entries: usize,
) -> Option<AskRuleSavePreview> {
    if rules.is_empty() {
        return None;
    }

    let entries = rules
        .iter()
        .take(max_entries)
        .map(format_ask_rule_save_entry)
        .collect();
    Some(AskRuleSavePreview {
        rule_count: rules.len(),
        entries,
        omitted: rules.len().saturating_sub(max_entries),
    })
}

#[must_use]
fn format_ask_rule_save_entry(rule: &ToolAskRule) -> String {
    let mut parts = vec![format!(
        "tool={}",
        sanitize_ask_rule_preview_value(&rule.tool)
    )];
    if let Some(command) = &rule.command {
        parts.push(format!(
            "command={}",
            sanitize_ask_rule_preview_value(command)
        ));
    }
    if let Some(path) = &rule.path {
        parts.push(format!("path={}", sanitize_ask_rule_preview_value(path)));
    }
    parts.join(" ")
}

#[must_use]
fn sanitize_ask_rule_preview_value(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\r', "\\r")
        .replace('\n', "\\n")
        .replace('\t', "\\t")
}

#[must_use]
pub(super) fn build_persistent_ask_rules(
    tool_name: &str,
    params: &Value,
    workspace: &Path,
) -> Vec<ToolAskRule> {
    match tool_name {
        "exec_shell" => build_exec_shell_ask_rules(params),
        // File writes save an exact, workspace-relative path so a later
        // edit/write of the same file is matched. read_file stays out: this
        // boundary is about persisting *write* approvals only.
        "write_file" | "edit_file" => build_write_file_ask_rules(tool_name, params, workspace),
        "apply_patch" => build_apply_patch_ask_rules(params, workspace),
        _ => Vec::new(),
    }
}

#[must_use]
fn build_exec_shell_ask_rules(params: &Value) -> Vec<ToolAskRule> {
    let Some(command) = params
        .get("command")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|command| !command.is_empty())
    else {
        return Vec::new();
    };
    vec![ToolAskRule::exec_shell(command)]
}

#[must_use]
fn build_write_file_ask_rules(
    tool_name: &str,
    params: &Value,
    workspace: &Path,
) -> Vec<ToolAskRule> {
    let Some(path) = params
        .get("path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|path| !path.is_empty())
    else {
        return Vec::new();
    };
    // Reuse the canonical matcher normalization so the saved rule equals what
    // runtime matching compares against. `None` (and the degenerate
    // workspace-root case) means the path is empty, traversing, drive-relative,
    // or outside the workspace, so we save nothing and the `S` shortcut and
    // preview stay disabled.
    let workspace = workspace.to_string_lossy();
    let Some(relative) =
        codewhale_execpolicy::normalize_workspace_relative_path(path, workspace.as_ref())
            .filter(|relative| !relative.is_empty())
    else {
        return Vec::new();
    };
    vec![ToolAskRule::file_path(tool_name, relative)]
}

#[must_use]
fn build_apply_patch_ask_rules(params: &Value, workspace: &Path) -> Vec<ToolAskRule> {
    let Ok(preflight) = crate::tools::apply_patch::preflight_apply_patch(params) else {
        return Vec::new();
    };
    let workspace = workspace.to_string_lossy();
    let mut rules = Vec::new();

    for path in preflight.touched_files {
        let Some(relative) =
            codewhale_execpolicy::normalize_workspace_relative_path(&path, workspace.as_ref())
                .filter(|relative| !relative.is_empty())
        else {
            return Vec::new();
        };
        let rule = ToolAskRule::file_path("apply_patch", relative);
        if !rules.contains(&rule) {
            rules.push(rule);
        }
    }

    rules
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const WORKSPACE: &str = "/workspace";

    fn rules_for(tool_name: &str, params: &Value) -> Vec<ToolAskRule> {
        build_persistent_ask_rules(tool_name, params, Path::new(WORKSPACE))
    }

    #[test]
    fn exec_shell_rule_saved_from_trimmed_command() {
        let rules = rules_for(
            "exec_shell",
            &json!({"command": " cargo test --workspace "}),
        );

        assert_eq!(
            rules,
            vec![ToolAskRule::exec_shell("cargo test --workspace")]
        );
    }

    #[test]
    fn exec_shell_rule_skipped_for_empty_command() {
        assert!(rules_for("exec_shell", &json!({"command": "   "})).is_empty());
        assert!(rules_for("exec_shell", &json!({})).is_empty());
    }

    #[test]
    fn ask_rule_save_preview_formats_shell_rule() {
        let rules = vec![ToolAskRule::exec_shell("cargo test --workspace")];

        let preview = build_ask_rule_save_preview(&rules).expect("save preview");
        assert_eq!(preview.rule_count, 1);
        assert_eq!(preview.summary(), "1 ask rule");
        assert_eq!(
            preview.entries,
            vec!["tool=exec_shell command=cargo test --workspace"]
        );
        assert_eq!(preview.omitted, 0);
    }

    #[test]
    fn write_file_ask_rule_saved_for_workspace_relative_path() {
        let rules = rules_for(
            "write_file",
            &json!({"path": "src/main.rs", "content": "test"}),
        );

        assert_eq!(
            rules,
            vec![ToolAskRule::file_path("write_file", "src/main.rs")]
        );
        let preview = build_ask_rule_save_preview(&rules).expect("save preview");
        assert_eq!(preview.entries, vec!["tool=write_file path=src/main.rs"]);
    }

    #[test]
    fn ask_rule_save_preview_formats_write_and_edit_file_paths() {
        let write = rules_for(
            "write_file",
            &json!({"path": "src/main.rs", "content": "test"}),
        );
        let edit = rules_for("edit_file", &json!({"path": "/workspace/src/lib.rs"}));

        assert_eq!(
            build_ask_rule_save_preview(&write)
                .expect("write save preview")
                .entries,
            vec!["tool=write_file path=src/main.rs"]
        );
        assert_eq!(
            build_ask_rule_save_preview(&edit)
                .expect("edit save preview")
                .entries,
            vec!["tool=edit_file path=src/lib.rs"]
        );
    }

    #[test]
    fn write_file_ask_rule_normalizes_absolute_path_to_workspace_relative() {
        let rules = rules_for("edit_file", &json!({"path": "/workspace/src/lib.rs"}));

        assert_eq!(
            rules,
            vec![ToolAskRule::file_path("edit_file", "src/lib.rs")]
        );
    }

    #[test]
    fn read_file_request_has_no_file_ask_rule() {
        assert!(rules_for("read_file", &json!({"path": "src/main.rs"})).is_empty());
    }

    #[test]
    fn write_file_ask_rule_skipped_for_unsafe_empty_or_external_paths() {
        for path in ["../escape.rs", "/etc/passwd", "   ", ""] {
            assert!(
                rules_for("write_file", &json!({"path": path})).is_empty(),
                "path {path:?} must not produce a rule"
            );
        }
    }

    #[test]
    fn apply_patch_ask_rules_saved_for_multi_file_patch() {
        let patch = r"diff --git a/src/a.rs b/src/a.rs
--- a/src/a.rs
+++ b/src/a.rs
@@ -1,1 +1,1 @@
-old
+new
diff --git a/src/b.rs b/src/b.rs
--- a/src/b.rs
+++ b/src/b.rs
@@ -1,1 +1,1 @@
-old
+new
";

        let rules = rules_for("apply_patch", &json!({"patch": patch}));

        assert_eq!(
            rules,
            vec![
                ToolAskRule::file_path("apply_patch", "src/a.rs"),
                ToolAskRule::file_path("apply_patch", "src/b.rs"),
            ]
        );
        let preview = build_ask_rule_save_preview(&rules).expect("save preview");
        assert_eq!(preview.summary(), "2 ask rules");
        assert_eq!(
            preview.entries,
            vec![
                "tool=apply_patch path=src/a.rs",
                "tool=apply_patch path=src/b.rs"
            ]
        );
    }

    #[test]
    fn apply_patch_ask_rules_dedupe_targets_after_normalization() {
        let rules = rules_for(
            "apply_patch",
            &json!({
                "changes": [
                    { "path": "src/a.rs", "content": "one" },
                    { "path": "/workspace/src/a.rs", "content": "two" }
                ]
            }),
        );

        assert_eq!(
            rules,
            vec![ToolAskRule::file_path("apply_patch", "src/a.rs")]
        );
    }

    #[test]
    fn apply_patch_ask_rule_handles_timestamp_headers() {
        let patch = "diff --git a/src/lib.rs b/src/lib.rs\n\
--- a/src/lib.rs\t2026-06-26 10:00:00 +0000\n\
+++ b/src/lib.rs\t2026-06-26 10:01:00 +0000\n\
@@ -1,1 +1,1 @@\n\
-old\n\
+new\n";

        let rules = rules_for("apply_patch", &json!({"patch": patch}));

        assert_eq!(
            rules,
            vec![ToolAskRule::file_path("apply_patch", "src/lib.rs")]
        );
    }

    #[test]
    fn apply_patch_ask_rule_ignores_forged_headers_inside_hunk() {
        let patch = r"--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,3 +1,3 @@
 line1
--- a/forged.rs
+++ b/forged.rs
 line3
";

        let rules = rules_for(
            "apply_patch",
            &json!({"path": "src/lib.rs", "patch": patch}),
        );

        assert_eq!(
            rules,
            vec![ToolAskRule::file_path("apply_patch", "src/lib.rs")]
        );
    }

    #[test]
    fn apply_patch_ask_rule_skipped_when_any_target_traverses_workspace() {
        let rules = rules_for(
            "apply_patch",
            &json!({
                "changes": [
                    { "path": "src/a.rs", "content": "safe" },
                    { "path": "../escape.rs", "content": "unsafe" }
                ]
            }),
        );

        assert!(rules.is_empty());
        assert_eq!(build_ask_rule_save_preview(&rules), None);
    }

    #[test]
    fn apply_patch_ask_rule_skipped_on_preflight_failure() {
        let rules = rules_for(
            "apply_patch",
            &json!({"patch": "@@ -1 +1 @@\n-old\n+new\n"}),
        );

        assert!(rules.is_empty());
        assert_eq!(build_ask_rule_save_preview(&rules), None);
    }

    #[test]
    fn ask_rule_save_preview_truncates_rule_list() {
        let rules = vec![
            ToolAskRule::file_path("apply_patch", "src/a.rs"),
            ToolAskRule::file_path("apply_patch", "src/b.rs"),
            ToolAskRule::file_path("apply_patch", "src/c.rs"),
            ToolAskRule::file_path("apply_patch", "src/d.rs"),
        ];

        let preview = build_ask_rule_save_preview_with_limit(&rules, 2).expect("save preview");
        assert_eq!(preview.rule_count, 4);
        assert_eq!(preview.summary(), "4 ask rules");
        assert_eq!(
            preview.entries,
            vec![
                "tool=apply_patch path=src/a.rs",
                "tool=apply_patch path=src/b.rs"
            ]
        );
        assert_eq!(preview.omitted, 2);
    }
}
