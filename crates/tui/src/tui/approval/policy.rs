use crate::command_safety::is_parallel_readonly_command;
use serde_json::Value;

/// Categorizes tools by cost/risk level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolCategory {
    /// Free, read-only operations (`list_dir`, `read_file`, todo_*)
    Safe,
    /// File modifications (`write_file`, `edit_file`)
    FileWrite,
    /// Shell execution (`exec_shell`)
    Shell,
    /// Network-oriented built-in tools
    Network,
    /// Read-only MCP discovery and resource access
    McpRead,
    /// MCP actions that may change remote state
    McpAction,
    /// Sub-agent lifecycle (`agent` start/status/peek/cancel); the child's
    /// own tool gates govern what it may actually do.
    Agent,
    /// Unknown or unclassified tool surface
    Unknown,
}

/// Stakes-based variant for the takeover modal.
///
/// `RiskLevel::Benign` lets a single keystroke commit the approval.
/// `RiskLevel::Destructive` keeps stronger warning copy and styling
/// around approvals that can touch files, shell, or remote state.
///
/// Routing rules live in [`classify_risk`] - when in doubt, route to
/// `Destructive`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RiskLevel {
    Benign,
    Destructive,
}

/// Presentation-level stakes for the approval prompt (#3883 follow-up).
///
/// `RiskLevel` drives keymaps and stays conservative ("not provably
/// read-only" is `Destructive`), but rendering everything in that bucket
/// as a red DESTRUCTIVE takeover made routine file edits and build
/// commands read like emergencies. Stakes split presentation three ways:
///
/// - `Routine` - provably read-only; minimal chrome.
/// - `Elevated` - ordinary state-touching work (edits, builds, MCP
///   actions); a calm approval, not a warning.
/// - `Critical` - genuinely destructive, publish-like, or
///   secret-touching per `ToolActionKind`; keeps the strong styling and
///   the policy semantics lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalStakes {
    Routine,
    Elevated,
    Critical,
}

/// Get the category for a tool by name.
pub fn get_tool_category(name: &str) -> ToolCategory {
    if name == "agent" {
        ToolCategory::Agent
    } else if matches!(name, "write_file" | "edit_file" | "apply_patch") {
        ToolCategory::FileWrite
    } else if matches!(
        name,
        "web_run" | "web_search" | "fetch_url" | "wait_for_dev_server"
    ) {
        ToolCategory::Network
    } else if matches!(
        name,
        "exec_shell"
            | "task_shell_start"
            | "task_shell_wait"
            | "exec_shell_wait"
            | "exec_shell_interact"
            | "exec_wait"
            | "exec_interact"
    ) {
        ToolCategory::Shell
    } else if name.starts_with("list_mcp_")
        || name.starts_with("read_mcp_")
        || name.starts_with("get_mcp_")
    {
        ToolCategory::McpRead
    } else if name.starts_with("mcp_") {
        ToolCategory::McpAction
    } else if matches!(
        name,
        "read_file"
            | "list_dir"
            | "todo_write"
            | "todo_read"
            | "note"
            | "update_plan"
            | "search"
            | "file_search"
            | "project"
            | "diagnostics"
    ) || name.starts_with("read_")
        || name.starts_with("list_")
        || name.starts_with("get_")
    {
        ToolCategory::Safe
    } else {
        ToolCategory::Unknown
    }
}

#[must_use]
pub fn classify_stakes(
    tool_name: &str,
    category: ToolCategory,
    risk: RiskLevel,
    params: &Value,
) -> ApprovalStakes {
    if matches!(risk, RiskLevel::Benign) {
        return ApprovalStakes::Routine;
    }
    match crate::tui::auto_review::ToolActionKind::from_tool_call(tool_name, params, category) {
        crate::tui::auto_review::ToolActionKind::Publish
        | crate::tui::auto_review::ToolActionKind::Destructive
        | crate::tui::auto_review::ToolActionKind::Secret => ApprovalStakes::Critical,
        _ => ApprovalStakes::Elevated,
    }
}

/// Decide the stakes variant for an approval request.
///
/// The bias is conservative: a category we don't recognise routes to
/// `Destructive`, and any shell command that `command_safety` flags as
/// `Dangerous` is forced to `Destructive` even when the rest of the
/// request looks calm. The split lets the modal render stronger warning
/// copy on anything that can touch state outside this turn.
#[must_use]
pub fn classify_risk(tool_name: &str, category: ToolCategory, params: &Value) -> RiskLevel {
    match category {
        // Read paths and discovery.
        ToolCategory::Safe | ToolCategory::McpRead => RiskLevel::Benign,
        // Query-only network is benign; opening a URL pulls arbitrary
        // remote content, so it stays destructive.
        ToolCategory::Network => match tool_name {
            "web_search" | "wait_for_dev_server" => RiskLevel::Benign,
            // web_run is benign for search/query, but its `open`/`click`
            // actions fetch model-supplied URLs (arbitrary remote content) -
            // destructive, consistent with fetch_url.
            "web_run" => {
                let fetches_url = params
                    .get("open")
                    .and_then(Value::as_array)
                    .is_some_and(|a| !a.is_empty())
                    || params
                        .get("click")
                        .and_then(Value::as_array)
                        .is_some_and(|a| !a.is_empty());
                if fetches_url {
                    RiskLevel::Destructive
                } else {
                    RiskLevel::Benign
                }
            }
            _ => RiskLevel::Destructive,
        },
        // Shell stays destructive unless the existing command-safety analyzer
        // can prove the concrete command is read-only.
        ToolCategory::Shell => {
            if let Some(cmd) = params.get("command").and_then(Value::as_str) {
                if is_parallel_readonly_command(cmd) {
                    return RiskLevel::Benign;
                }
            }
            RiskLevel::Destructive
        }
        // Sub-agent lifecycle: status/peek are inspection-only. Starts and
        // other actions keep the explicit-options keymap (the child's own
        // gates govern what it may do once running).
        ToolCategory::Agent => match params.get("action").and_then(Value::as_str) {
            Some("status" | "peek" | "list") => RiskLevel::Benign,
            _ => RiskLevel::Destructive,
        },
        // File writes, MCP actions, unclassified surfaces - all
        // require explicit confirmation.
        ToolCategory::FileWrite | ToolCategory::McpAction | ToolCategory::Unknown => {
            RiskLevel::Destructive
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_get_tool_category_safe_tools() {
        assert_eq!(get_tool_category("read_file"), ToolCategory::Safe);
        assert_eq!(get_tool_category("list_dir"), ToolCategory::Safe);
        assert_eq!(get_tool_category("todo_write"), ToolCategory::Safe);
        assert_eq!(get_tool_category("todo_read"), ToolCategory::Safe);
        assert_eq!(get_tool_category("note"), ToolCategory::Safe);
        assert_eq!(get_tool_category("update_plan"), ToolCategory::Safe);
    }

    #[test]
    fn test_get_tool_category_file_write_tools() {
        assert_eq!(get_tool_category("write_file"), ToolCategory::FileWrite);
        assert_eq!(get_tool_category("edit_file"), ToolCategory::FileWrite);
        assert_eq!(get_tool_category("apply_patch"), ToolCategory::FileWrite);
    }

    #[test]
    fn test_get_tool_category_shell_tools() {
        assert_eq!(get_tool_category("exec_shell"), ToolCategory::Shell);
        assert_eq!(get_tool_category("task_shell_start"), ToolCategory::Shell);
        assert_eq!(get_tool_category("task_shell_wait"), ToolCategory::Shell);
        assert_eq!(get_tool_category("exec_shell_wait"), ToolCategory::Shell);
        assert_eq!(
            get_tool_category("exec_shell_interact"),
            ToolCategory::Shell
        );
        assert_eq!(get_tool_category("exec_wait"), ToolCategory::Shell);
        assert_eq!(get_tool_category("exec_interact"), ToolCategory::Shell);
        assert_eq!(
            get_tool_category("mcp_linear_save_issue"),
            ToolCategory::McpAction
        );
        assert_eq!(get_tool_category("list_mcp_tools"), ToolCategory::McpRead);
    }

    #[test]
    fn test_get_tool_category_agent_tool() {
        assert_eq!(get_tool_category("agent"), ToolCategory::Agent);
    }

    #[test]
    fn test_get_tool_category_unknown_tools_need_review() {
        assert_eq!(get_tool_category("unknown_tool"), ToolCategory::Unknown);
    }

    #[test]
    fn risk_safe_categories_route_benign() {
        let cat = ToolCategory::Safe;
        assert_eq!(
            classify_risk("read_file", cat, &json!({"path": "x"})),
            RiskLevel::Benign
        );
        let cat = ToolCategory::McpRead;
        assert_eq!(
            classify_risk("list_mcp_tools", cat, &json!({})),
            RiskLevel::Benign
        );
    }

    #[test]
    fn risk_query_only_network_is_benign_but_fetch_is_destructive() {
        // web_search is read-only enough to use the benign variant.
        let cat = ToolCategory::Network;
        assert_eq!(
            classify_risk("web_search", cat, &json!({"q": "rust"})),
            RiskLevel::Benign
        );
        // fetch_url pulls arbitrary remote content, so it stays destructive.
        assert_eq!(
            classify_risk("fetch_url", cat, &json!({"url": "https://example.com"})),
            RiskLevel::Destructive
        );
        // wait_for_dev_server only permits loopback targets.
        assert_eq!(
            classify_risk("wait_for_dev_server", cat, &json!({"port": 5173})),
            RiskLevel::Benign
        );
    }

    #[test]
    fn risk_writes_shell_mcp_action_unknown_route_destructive() {
        for (name, cat) in [
            ("write_file", ToolCategory::FileWrite),
            ("edit_file", ToolCategory::FileWrite),
            ("apply_patch", ToolCategory::FileWrite),
            ("exec_shell", ToolCategory::Shell),
            ("mcp_linear_save_issue", ToolCategory::McpAction),
            ("totally_new_tool", ToolCategory::Unknown),
        ] {
            assert_eq!(
                classify_risk(name, cat, &json!({})),
                RiskLevel::Destructive,
                "expected {name:?} to be Destructive",
            );
        }
    }

    #[test]
    fn risk_read_only_shell_commands_route_benign() {
        let cat = ToolCategory::Shell;
        for command in [
            "codewhale --version",
            "codewhale --help",
            "git status --porcelain",
        ] {
            assert_eq!(
                classify_risk("exec_shell", cat, &json!({ "command": command })),
                RiskLevel::Benign,
                "expected read-only shell command {command:?} to be Benign",
            );
        }
    }

    #[test]
    fn risk_dangerous_shell_command_stays_destructive() {
        // command_safety would flag this as Dangerous; classify_risk
        // already routes Shell to Destructive. The check exists so a
        // future attempt to relax shell to Benign cannot smuggle this
        // through unexamined.
        let cat = ToolCategory::Shell;
        assert_eq!(
            classify_risk("exec_shell", cat, &json!({"command": "rm -rf /"})),
            RiskLevel::Destructive
        );
    }

    #[test]
    fn web_run_risk_is_param_aware() {
        // search/query is benign; open/click fetch arbitrary URLs -> destructive.
        assert_eq!(
            classify_risk("web_run", ToolCategory::Network, &json!({"search": "rust"})),
            RiskLevel::Benign
        );
        assert_eq!(
            classify_risk(
                "web_run",
                ToolCategory::Network,
                &json!({"open": [{"ref": "https://evil.example"}]})
            ),
            RiskLevel::Destructive
        );
        assert_eq!(
            classify_risk(
                "web_run",
                ToolCategory::Network,
                &json!({"click": [{"ref": "1"}]})
            ),
            RiskLevel::Destructive
        );
    }

    #[test]
    fn stakes_split_routine_elevated_critical() {
        assert_eq!(
            classify_stakes(
                "read_file",
                ToolCategory::Safe,
                RiskLevel::Benign,
                &json!({"path": "src/main.rs"})
            ),
            ApprovalStakes::Routine
        );
        assert_eq!(
            classify_stakes(
                "write_file",
                ToolCategory::FileWrite,
                RiskLevel::Destructive,
                &json!({"path": "src/main.rs", "content": "test"})
            ),
            ApprovalStakes::Elevated
        );
        assert_eq!(
            classify_stakes(
                "exec_shell",
                ToolCategory::Shell,
                RiskLevel::Destructive,
                &json!({"command": "cargo test --workspace"})
            ),
            ApprovalStakes::Elevated
        );
        assert_eq!(
            classify_stakes(
                "exec_shell",
                ToolCategory::Shell,
                RiskLevel::Destructive,
                &json!({"command": "rm -rf ~/"})
            ),
            ApprovalStakes::Critical
        );
        // Publish-like shell is critical in every origin.
        assert_eq!(
            classify_stakes(
                "exec_shell",
                ToolCategory::Shell,
                RiskLevel::Destructive,
                &json!({"command": "git push origin main"})
            ),
            ApprovalStakes::Critical
        );
    }

    #[test]
    fn agent_status_and_peek_are_benign() {
        for action in ["status", "peek", "list"] {
            let params = json!({"action": action, "agent_id": "agent_1"});
            let risk = classify_risk("agent", ToolCategory::Agent, &params);
            assert_eq!(risk, RiskLevel::Benign, "{action}");
            assert_eq!(
                classify_stakes("agent", ToolCategory::Agent, risk, &params),
                ApprovalStakes::Routine,
                "{action}"
            );
        }
    }
}
