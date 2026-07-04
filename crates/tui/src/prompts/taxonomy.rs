//! Core model-facing tool taxonomy rendering.

use crate::tui::app::AppMode;

pub(crate) const TOOL_TAXONOMY_DISCOVERY: &[&str] = &["grep_files", "file_search"];
pub(crate) const TOOL_TAXONOMY_GIT: &[&str] = &["git_status", "git_diff"];
pub(crate) const TOOL_TAXONOMY_VERIFICATION: &[&str] = &["run_tests", "run_verifiers"];

/// Return the core tool taxonomy body **without** a markdown heading.
/// Suitable for embedding under a mode-specific sub-heading in the
/// Runtime Policy Reference without producing a broken heading hierarchy.
pub(crate) fn render_core_tool_taxonomy_body(mode: AppMode) -> String {
    let core_tools = core_taxonomy_tools_for_mode(mode);
    let mut sentences = Vec::new();

    if let Some(discovery) = render_core_tool_group(TOOL_TAXONOMY_DISCOVERY, &core_tools) {
        sentences.push(format!("Use {discovery} for discovery."));
    }
    if let Some(git) = render_core_tool_group(TOOL_TAXONOMY_GIT, &core_tools) {
        sentences.push(format!("Use {git} for git inspection."));
    }
    if let Some(verification) = render_core_tool_group(TOOL_TAXONOMY_VERIFICATION, &core_tools) {
        sentences.push(format!("Use {verification} for verification."));
    }
    if core_tools.contains(&"run_verifiers") {
        sentences.push(
            "For long build/test/lint verifier suites, call `run_verifiers` with `background: true` or use `task_shell_start`, then poll while continuing independent inspection."
                .to_string(),
        );
    }

    debug_assert!(
        !sentences.is_empty(),
        "core tool taxonomy has no active tool groups"
    );
    sentences.join(" ")
}

fn core_taxonomy_tools_for_mode(mode: AppMode) -> Vec<&'static str> {
    let core_tools = crate::core::engine::default_active_native_tool_names();
    core_tools
        .iter()
        .copied()
        .filter(|tool| mode != AppMode::Plan || !matches!(*tool, "run_tests" | "run_verifiers"))
        .collect()
}

fn render_core_tool_group(group: &[&str], core_tools: &[&str]) -> Option<String> {
    let rendered = group
        .iter()
        .copied()
        .filter(|tool| core_tools.contains(tool))
        .map(|tool| format!("`{tool}`"))
        .collect::<Vec<_>>()
        .join("/");
    (!rendered.is_empty()).then_some(rendered)
}
