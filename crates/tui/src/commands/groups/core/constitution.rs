//! `/constitution` command surface (#3806).

use std::fmt::Write as _;
use std::path::PathBuf;

use codewhale_config::{
    ConstitutionChoice, ConstitutionSource, ConstitutionValidity, RuntimePostureSource, SetupState,
    SetupStep, UserConstitution, UserConstitutionLoad,
};

use crate::commands::traits::{CommandInfo, RegisterCommand};
use crate::localization::MessageId;
use crate::tui::app::{App, AppAction};
use crate::tui::pager::PagerView;

use super::CommandResult;

pub(in crate::commands) const COMMAND_INFO: CommandInfo = CommandInfo {
    name: "constitution",
    aliases: &["law"],
    usage: "/constitution [status|preview|bundled|edit|review|repair|repo|explain|posture]",
    description_id: MessageId::CmdConstitutionDescription,
};

pub(in crate::commands) struct ConstitutionCmd;

impl RegisterCommand for ConstitutionCmd {
    fn info() -> &'static CommandInfo {
        &COMMAND_INFO
    }

    fn execute(app: &mut App, arg: Option<&str>) -> CommandResult {
        match arg.map(str::trim).filter(|arg| !arg.is_empty()) {
            None | Some("status" | "home" | "manager") => {
                open_status(app);
                CommandResult::ok()
            }
            Some("preview") => {
                open_preview(app);
                CommandResult::ok()
            }
            Some("review" | "existing") => {
                open_review(app);
                CommandResult::ok()
            }
            Some("repo" | "repo-local" | "law") => {
                open_repo_law(app);
                CommandResult::ok()
            }
            Some("explain" | "agents") => {
                open_explanation(app);
                CommandResult::ok()
            }
            Some("edit" | "guided" | "custom") => {
                CommandResult::action(AppAction::OpenSetupWizardAt {
                    step: SetupStep::Constitution,
                })
            }
            Some("repair" | "fix") => CommandResult::action(AppAction::OpenSetupWizardAt {
                step: SetupStep::Constitution,
            }),
            Some("posture" | "runtime-posture") => {
                CommandResult::action(AppAction::OpenSetupWizardAt {
                    step: SetupStep::TrustSandbox,
                })
            }
            Some("bundled" | "default" | "use-bundled" | "use-default") => {
                CommandResult::action(AppAction::UseBundledConstitution)
            }
            Some("help") => CommandResult::message(help_text()),
            Some(other) => CommandResult::error(format!(
                "Unknown /constitution target '{other}'. Try `/constitution` for the manager."
            )),
        }
    }
}

fn open_status(app: &mut App) {
    let text = format_status(app);
    open_pager(app, "Constitution", &text);
}

fn open_review(app: &mut App) {
    let mut text = format_status(app);
    let _ = write!(text, "\n\n{}", preview_text());
    open_pager(app, "Constitution Review", &text);
}

fn open_preview(app: &mut App) {
    let text = preview_text();
    open_pager(app, "Rendered User Constitution", &text);
}

fn open_repo_law(app: &mut App) {
    let context = crate::project_context::load_project_context_with_parents(&app.workspace);
    let text = match context.constitution_block {
        Some(block) => block,
        None => {
            "No repo-local constitution found at .codewhale/constitution.json for this workspace."
                .to_string()
        }
    };
    open_pager(app, "Repo-Local Constitution", &text);
}

fn open_explanation(app: &mut App) {
    open_pager(app, "AGENTS.md vs Constitution", AGENTS_EXPLANATION);
}

fn open_pager(app: &mut App, title: &str, text: &str) {
    let width = app
        .viewport
        .last_transcript_area
        .map(|area| area.width)
        .unwrap_or(80);
    app.view_stack
        .push(PagerView::from_text(title, text, width.saturating_sub(2)));
}

fn format_status(app: &App) -> String {
    let state = load_setup_state();
    let load = load_user_constitution();
    let context = crate::project_context::load_project_context_with_parents(&app.workspace);
    let mut out = String::new();

    out.push_str("Constitution Manager\n\n");
    out.push_str("Active stack\n");
    out.push_str("- Bundled Constitution: active base law (always on)\n");
    let _ = writeln!(
        out,
        "- User-global constitution: {}",
        user_constitution_stack_status(state.as_ref(), &load)
    );
    if let Some(path) = context.constitution_source_path.as_ref() {
        let _ = writeln!(
            out,
            "- Repo-local constitution: present ({})",
            path.display()
        );
    } else {
        out.push_str("- Repo-local constitution: not present\n");
    }
    if let Some(path) = context.source_path.as_ref() {
        let _ = writeln!(
            out,
            "- AGENTS/project instructions: present ({})",
            path.display()
        );
    } else if context.instructions.is_some() {
        out.push_str("- AGENTS/project instructions: generated fallback\n");
    } else {
        out.push_str("- AGENTS/project instructions: not present\n");
    }
    let whale_warnings = ignored_whale_warnings(&context.warnings);
    if whale_warnings.is_empty() {
        out.push_str("- Legacy WHALE.md: not present\n");
    } else {
        let _ = writeln!(
            out,
            "- Legacy WHALE.md: ignored; migration needed ({} location{})",
            whale_warnings.len(),
            if whale_warnings.len() == 1 { "" } else { "s" }
        );
        for warning in whale_warnings {
            let _ = writeln!(out, "  - {warning}");
        }
    }
    let handoff_path = app.workspace.join(crate::prompts::HANDOFF_RELATIVE_PATH);
    let _ = writeln!(
        out,
        "- Memory/handoff: memory {}, handoff {}",
        if app.use_memory {
            "enabled"
        } else {
            "disabled"
        },
        if handoff_path.exists() {
            "present"
        } else {
            "not present"
        }
    );

    out.push_str("\nUser-global constitution\n");
    let _ = writeln!(
        out,
        "- Choice: {}",
        state
            .as_ref()
            .map_or("not recorded", |s| choice_label(s.constitution_choice))
    );
    let _ = writeln!(
        out,
        "- Source: {}",
        state
            .as_ref()
            .map_or("not recorded", |s| source_label(s.constitution_source))
    );
    let _ = writeln!(out, "- File: {}", user_constitution_file_label(&load));
    let _ = writeln!(
        out,
        "- Validity: {}",
        validity_label(load.validity_for_display(state.as_ref()))
    );
    let _ = writeln!(
        out,
        "- Language: {}",
        constitution_language(state.as_ref(), &load)
    );
    let _ = writeln!(
        out,
        "- Last accepted preview: {}",
        preview_record_label(state.as_ref())
    );
    let _ = writeln!(
        out,
        "- Runtime posture: {}",
        state
            .as_ref()
            .map_or("not reviewed", |s| posture_label(s.runtime_posture_source))
    );
    let _ = writeln!(
        out,
        "- Checkpoint: {}",
        state.as_ref().map_or("not completed".to_string(), |s| {
            s.constitution_checkpoint_completed_for
                .as_ref()
                .map_or_else(
                    || "not completed".to_string(),
                    |v| format!("completed for {v}"),
                )
        })
    );

    out.push_str("\nPreview\n");
    out.push_str(
        "- /constitution preview opens the exact rendered user-global block when present.\n",
    );
    out.push_str(
        "- /constitution repo shows .codewhale/constitution.json local law when present.\n",
    );

    out.push_str("\nMaintenance\n");
    out.push_str("- Edit guided constitution: /constitution edit\n");
    out.push_str("- Preview rendered constitution: /constitution preview\n");
    out.push_str("- Use bundled/default: /constitution bundled\n");
    out.push_str("- Review existing: /constitution review\n");
    out.push_str("- Repair invalid/empty/unreadable: /constitution repair\n");
    out.push_str("- Show repo-local law: /constitution repo\n");
    out.push_str("- Explain AGENTS.md vs constitution: /constitution explain\n");
    out.push_str("- Open runtime posture: /constitution posture\n");
    out
}

fn preview_text() -> String {
    let state = load_setup_state();
    let load = load_user_constitution();
    match load {
        UserConstitutionStatus::Loaded { path, constitution } => {
            let active = user_constitution_is_active(state.as_ref());
            let mut text = String::new();
            if !active {
                text.push_str(
                    "Inactive preview: bundled/default or expert override is selected.\n\n",
                );
            }
            text.push_str(
                &constitution
                    .render_block(Some(&path))
                    .unwrap_or_else(|| "The structured constitution is empty.".to_string()),
            );
            text
        }
        UserConstitutionStatus::Missing { path } => {
            format!(
                "No structured user-global constitution found at {}.\n\nBundled law applies. Use /constitution edit to create guided standing preferences, or /constitution bundled to record bundled/default explicitly.",
                path.display()
            )
        }
        UserConstitutionStatus::Empty { path } => {
            format!(
                "The structured user-global constitution at {} is empty. Use /constitution repair to return to the guided constitution step.",
                path.display()
            )
        }
        UserConstitutionStatus::Invalid { path, error } => {
            format!(
                "The structured user-global constitution at {} is invalid and is not injected.\n\n{error}\n\nUse /constitution repair to return to the guided constitution step.",
                path.display()
            )
        }
        UserConstitutionStatus::Unreadable { path, error } => {
            format!(
                "The structured user-global constitution at {} could not be read and is not injected.\n\n{error}\n\nUse /constitution repair to return to the guided constitution step.",
                path.display()
            )
        }
        UserConstitutionStatus::PathError { error } => {
            format!("Could not resolve CODEWHALE_HOME for the user-global constitution:\n\n{error}")
        }
    }
}

fn load_setup_state() -> Option<SetupState> {
    SetupState::load().ok().flatten()
}

#[derive(Debug)]
enum UserConstitutionStatus {
    Missing {
        path: PathBuf,
    },
    Empty {
        path: PathBuf,
    },
    Invalid {
        path: PathBuf,
        error: String,
    },
    Unreadable {
        path: PathBuf,
        error: String,
    },
    Loaded {
        path: PathBuf,
        constitution: Box<codewhale_config::UserConstitution>,
    },
    PathError {
        error: String,
    },
}

impl UserConstitutionStatus {
    fn validity(&self) -> ConstitutionValidity {
        match self {
            Self::Missing { .. } | Self::PathError { .. } => ConstitutionValidity::Unknown,
            Self::Empty { .. } => ConstitutionValidity::Empty,
            Self::Invalid { .. } => ConstitutionValidity::Invalid,
            Self::Unreadable { .. } => ConstitutionValidity::Unreadable,
            Self::Loaded { constitution, .. } => constitution.validity(),
        }
    }

    fn validity_for_display(&self, state: Option<&SetupState>) -> ConstitutionValidity {
        match self {
            Self::Missing { .. } | Self::PathError { .. } => {
                state.map_or(ConstitutionValidity::Unknown, |s| s.constitution_validity)
            }
            _ => self.validity(),
        }
    }
}

fn load_user_constitution() -> UserConstitutionStatus {
    let path = match UserConstitution::path() {
        Ok(path) => path,
        Err(error) => {
            return UserConstitutionStatus::PathError {
                error: error.to_string(),
            };
        }
    };

    match UserConstitution::load_from(&path) {
        UserConstitutionLoad::Missing => UserConstitutionStatus::Missing { path },
        UserConstitutionLoad::Empty => UserConstitutionStatus::Empty { path },
        UserConstitutionLoad::Invalid(error) => UserConstitutionStatus::Invalid { path, error },
        UserConstitutionLoad::Unreadable(error) => {
            UserConstitutionStatus::Unreadable { path, error }
        }
        UserConstitutionLoad::Loaded(constitution) => {
            UserConstitutionStatus::Loaded { path, constitution }
        }
    }
}

fn user_constitution_stack_status(
    state: Option<&SetupState>,
    load: &UserConstitutionStatus,
) -> String {
    match load {
        UserConstitutionStatus::Loaded { .. } if user_constitution_is_active(state) => {
            "active structured user-global law".to_string()
        }
        UserConstitutionStatus::Loaded { .. } => {
            "valid but inactive (bundled/default or expert override selected)".to_string()
        }
        UserConstitutionStatus::Missing { .. } => {
            "not configured; bundled/default applies".to_string()
        }
        UserConstitutionStatus::Empty { .. } => "empty; repair recommended".to_string(),
        UserConstitutionStatus::Invalid { .. } => "invalid; repair recommended".to_string(),
        UserConstitutionStatus::Unreadable { .. } => "unreadable; repair recommended".to_string(),
        UserConstitutionStatus::PathError { .. } => "unavailable; CODEWHALE_HOME error".to_string(),
    }
}

fn user_constitution_is_active(state: Option<&SetupState>) -> bool {
    !matches!(
        state.map(|s| s.constitution_choice),
        Some(
            ConstitutionChoice::Bundled
                | ConstitutionChoice::Deferred
                | ConstitutionChoice::ExpertOverride
        )
    )
}

fn user_constitution_file_label(load: &UserConstitutionStatus) -> String {
    match load {
        UserConstitutionStatus::Missing { path }
        | UserConstitutionStatus::Empty { path }
        | UserConstitutionStatus::Invalid { path, .. }
        | UserConstitutionStatus::Unreadable { path, .. }
        | UserConstitutionStatus::Loaded { path, .. } => path.display().to_string(),
        UserConstitutionStatus::PathError { .. } => "unresolved".to_string(),
    }
}

fn constitution_language(state: Option<&SetupState>, load: &UserConstitutionStatus) -> String {
    if let UserConstitutionStatus::Loaded { constitution, .. } = load
        && let Some(language) = constitution.language.as_deref()
    {
        return language.to_string();
    }
    state
        .and_then(|s| s.constitution_language.as_deref())
        .unwrap_or("not recorded")
        .to_string()
}

fn preview_record_label(state: Option<&SetupState>) -> String {
    let Some(state) = state else {
        return "not recorded".to_string();
    };
    match state.constitution_preview_hash.as_deref() {
        Some(hash) => format!("v{} ({hash})", state.constitution_preview_version),
        None => "not recorded".to_string(),
    }
}

fn choice_label(choice: ConstitutionChoice) -> &'static str {
    match choice {
        ConstitutionChoice::Unset => "not set",
        ConstitutionChoice::Bundled => "bundled/default",
        ConstitutionChoice::GuidedCustom => "guided custom",
        ConstitutionChoice::ExpertOverride => "expert override",
        ConstitutionChoice::Deferred => "deferred; bundled applies",
    }
}

fn source_label(source: ConstitutionSource) -> &'static str {
    match source {
        ConstitutionSource::Bundled => "bundled",
        ConstitutionSource::UserGlobal => "user-global constitution.json",
        ConstitutionSource::ExpertOverride => "expert prompt override",
    }
}

fn validity_label(validity: ConstitutionValidity) -> &'static str {
    match validity {
        ConstitutionValidity::Unknown => "unknown or not custom",
        ConstitutionValidity::Valid => "valid",
        ConstitutionValidity::Invalid => "invalid",
        ConstitutionValidity::Empty => "empty",
        ConstitutionValidity::Unreadable => "unreadable",
    }
}

fn posture_label(source: RuntimePostureSource) -> &'static str {
    match source {
        RuntimePostureSource::Unset => "not reviewed",
        RuntimePostureSource::Inherited => "inherited from existing config",
        RuntimePostureSource::Confirmed => "confirmed in setup",
    }
}

fn ignored_whale_warnings(warnings: &[String]) -> Vec<&str> {
    warnings
        .iter()
        .map(String::as_str)
        .filter(|warning| warning.contains("WHALE.md is ignored"))
        .collect()
}

fn help_text() -> String {
    "Usage: /constitution [status|preview|bundled|edit|review|repair|repo|explain|posture]"
        .to_string()
}

const AGENTS_EXPLANATION: &str = "\
AGENTS.md vs constitution

The bundled Constitution is the global system contract: identity, authority order, safety, and the standing execution rules.

The user-global constitution is personal standing law. It is structured, rendered deterministically, and subordinate to the current user request and the bundled Constitution.

.codewhale/constitution.json is repo-local law. It belongs to a workspace and is rendered as a separate repo constitution block.

AGENTS.md and project instructions are implementation guidance. They can describe build commands, repository norms, and local workflows, but they should not replace constitution policy or raw full-prompt editing.

WHALE.md is ignored. Move ordinary project instructions to AGENTS.md and CodeWhale-specific authority policy to .codewhale/constitution.json.

Runtime posture is separate. A constitution can recommend autonomy, but it does not change approval policy, sandbox, shell, network, trust, MCP permissions, or default mode. Use /constitution posture to review those controls.";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::tui::app::TuiOptions;
    use crate::tui::pager::PagerView;
    use crate::tui::views::ModalKind;
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn test_app() -> App {
        test_app_with_workspace(PathBuf::from("."))
    }

    fn test_app_with_workspace(workspace: PathBuf) -> App {
        let options = TuiOptions {
            model: "deepseek-v4-pro".to_string(),
            workspace,
            config_path: None,
            config_profile: None,
            allow_shell: false,
            use_alt_screen: true,
            use_mouse_capture: false,
            use_bracketed_paste: true,
            max_subagents: 1,
            skills_dir: PathBuf::from("."),
            memory_path: PathBuf::from("memory.md"),
            notes_path: PathBuf::from("notes.txt"),
            mcp_config_path: PathBuf::from("mcp.json"),
            use_memory: false,
            start_in_agent_mode: false,
            skip_onboarding: true,
            yolo: false,
            resume_session_id: None,
            initial_input: None,
        };
        App::new(options, &Config::default())
    }

    fn pop_pager_body(app: &mut App) -> String {
        let mut view = app.view_stack.pop().expect("pager view");
        let pager = view
            .as_any_mut()
            .downcast_mut::<PagerView>()
            .expect("top view should be pager");
        pager.body_text()
    }

    #[test]
    fn constitution_default_opens_manager_pager() {
        let mut app = test_app();

        let result = ConstitutionCmd::execute(&mut app, None);

        assert!(result.message.is_none());
        assert_eq!(app.view_stack.top_kind(), Some(ModalKind::Pager));
        assert!(pop_pager_body(&mut app).contains("Constitution Manager"));
    }

    #[test]
    fn constitution_manager_marks_whale_md_ignored() {
        let tmp = tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("WHALE.md"), "legacy instructions").expect("write whale");
        let mut app = test_app_with_workspace(tmp.path().to_path_buf());

        let result = ConstitutionCmd::execute(&mut app, None);

        assert!(result.message.is_none());
        let body = pop_pager_body(&mut app);
        assert!(body.contains("Legacy WHALE.md: ignored"));
        assert!(body.contains("WHALE.md is ignored"));
        assert!(!body.contains("legacy instructions"));
    }

    #[test]
    fn constitution_bundled_emits_action() {
        let mut app = test_app();

        let result = ConstitutionCmd::execute(&mut app, Some("bundled"));

        assert_eq!(result.action, Some(AppAction::UseBundledConstitution));
    }

    #[test]
    fn constitution_edit_opens_setup_at_constitution() {
        let mut app = test_app();

        let result = ConstitutionCmd::execute(&mut app, Some("edit"));

        assert_eq!(
            result.action,
            Some(AppAction::OpenSetupWizardAt {
                step: SetupStep::Constitution
            })
        );
    }

    #[test]
    fn constitution_preview_renders_structured_block() {
        let _env_guard = crate::test_support::lock_test_env();
        let tmp = tempdir().expect("tempdir");
        let home = tmp.path().join("codewhale-home");
        std::fs::create_dir_all(&home).expect("home");
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", home.as_os_str());
        let constitution = UserConstitution {
            about: Some("Maintains release lanes.".to_string()),
            ..UserConstitution::default()
        };
        constitution.save().expect("save constitution");
        let mut app = test_app();

        let result = ConstitutionCmd::execute(&mut app, Some("preview"));

        assert!(result.message.is_none());
        let body = pop_pager_body(&mut app);
        assert!(body.contains("<codewhale_user_constitution"));
        assert!(body.contains("Maintains release lanes."));
    }
}
