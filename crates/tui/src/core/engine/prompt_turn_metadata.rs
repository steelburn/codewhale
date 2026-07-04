//! Transient turn metadata rendered into user-role request tails.
//!
//! The engine gathers live state; this module owns the model-visible formatting
//! so prompt metadata can be reviewed without spelunking through the turn path.

use std::path::Path;

use crate::core::ops::UserInputProvenance;
use crate::models::ContentBlock;
use crate::tui::app::AppMode;

use super::turn_policy::PolicyNarrowingEvent;

pub(super) struct TurnMetadataInput<'a> {
    pub today: String,
    pub workspace: &'a Path,
    pub routed_model: &'a str,
    pub mode: AppMode,
    pub mode_instructions: &'static str,
    pub provenance: UserInputProvenance,
    pub auto_model: bool,
    pub reasoning_effort: Option<&'a str>,
    pub reasoning_effort_auto: bool,
    pub policy_narrowing: Option<&'a PolicyNarrowingEvent>,
    pub resource_metadata_lines: Vec<String>,
    pub working_set_summary: Option<String>,
}

pub(super) fn build_turn_metadata_block(input: TurnMetadataInput<'_>) -> ContentBlock {
    let mut lines = vec![
        format!("Current local date: {}", input.today),
        // Workspace path moved here from the static `## Environment` block so
        // the static system prefix stays byte-stable across sessions.
        format!("Current workspace: {}", input.workspace.display()),
        format!("Current model: {}", input.routed_model),
        format!("Current mode: {}", input.mode.as_setting()),
        "Current mode policy source: runtime".to_string(),
        format!("Current mode policy:\n{}", input.mode_instructions),
        format!("Input provenance: {}", input.provenance.as_str()),
        format!(
            "Input authority: {}",
            if input.provenance.can_authorize_work() {
                "external_current_turn"
            } else {
                "non_authoritative"
            }
        ),
    ];
    if input.auto_model {
        lines.push(format!("Auto model route: {}", input.routed_model));
    }
    if input.reasoning_effort_auto
        && let Some(reasoning_effort) = input.reasoning_effort
    {
        lines.push(format!("Auto reasoning effort: {reasoning_effort}"));
    }
    if let Some(event) = input.policy_narrowing {
        lines.push(format!("Policy narrowing: {}", event.reason().as_str()));
        lines.push(format!("Policy narrowing status: {}", event.message()));
    }
    lines.extend(input.resource_metadata_lines);
    if let Some(working_set_summary) = input.working_set_summary {
        lines.push(working_set_summary);
    }
    let summary = lines.join("\n");

    ContentBlock::Text {
        text: format!("<turn_meta>\n{summary}\n</turn_meta>"),
        cache_control: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_block_renders_runtime_mode_and_provenance() {
        let block = build_turn_metadata_block(TurnMetadataInput {
            today: "2026-07-03".to_string(),
            workspace: Path::new("/tmp/codewhale"),
            routed_model: "deepseek-v4-pro",
            mode: AppMode::Plan,
            mode_instructions: "Plan mode instructions",
            provenance: UserInputProvenance::ExternalUser,
            auto_model: true,
            reasoning_effort: Some("max"),
            reasoning_effort_auto: true,
            policy_narrowing: None,
            resource_metadata_lines: vec!["Active goal resource usage: 42 tokens".to_string()],
            working_set_summary: Some("## Repo Working Set\n- src/main.rs".to_string()),
        });
        let ContentBlock::Text { text, .. } = block else {
            panic!("expected text block");
        };

        assert!(text.contains("Current local date: 2026-07-03"));
        assert!(text.contains("Current workspace: /tmp/codewhale"));
        assert!(text.contains("Current mode: plan"));
        assert!(text.contains("Current mode policy:\nPlan mode instructions"));
        assert!(text.contains("Input authority: external_current_turn"));
        assert!(text.contains("Auto model route: deepseek-v4-pro"));
        assert!(text.contains("Auto reasoning effort: max"));
        assert!(text.contains("Active goal resource usage: 42 tokens"));
        assert!(text.contains("## Repo Working Set"));
    }
}
