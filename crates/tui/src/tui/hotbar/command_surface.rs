//! Hotbar command surface — coordination layer for state management,
//! binding persistence, command resolution, and status display.
//!
//! This module ties together the action registry, config resolution,
//! dispatch, and sidebar rendering into a single surface consumed by
//! the TUI event loop and sidebar renderer.

use std::collections::BTreeMap;

use crate::config::Config;
use crate::tui::app::App;
use crate::tui::hotbar::actions::{
    HotbarActionMetadata, HotbarDispatch, HotbarRecommendationOptions, recommend_hotbar_actions,
    recommended_hotbar_bindings,
};

/// Resolved state for a single hotbar slot (1–8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotbarSlotState {
    /// Slot number (1-based).
    pub slot: u8,
    /// Resolved action id, if any.
    pub action_id: Option<String>,
    /// Display label for the slot cell.
    pub label: String,
    /// Full descriptive text for hover tooltips.
    pub full_text: String,
    /// Whether this slot's action is currently active.
    pub active: bool,
    /// Whether the action is unknown to the registry.
    pub unknown: bool,
    /// Whether the slot is bound to a disabled action.
    pub disabled_reason: Option<String>,
}

/// Central hotbar command surface.
///
/// Owns the resolved binding state and provides methods for dispatch,
/// status display, and persistence coordination. The TUI event loop
/// creates one instance per render cycle and destroys it; this is
/// intentionally cheap and allocates no long-lived caches.
#[derive(Debug, Clone)]
pub struct HotbarCommandSurface {
    /// Resolved bindings keyed by slot (1-based).
    bindings: BTreeMap<u8, codewhale_config::HotbarBinding>,
    /// Whether the hotbar is explicitly disabled (`hotbar = []`).
    explicitly_disabled: bool,
    /// Whether the hotbar has any bindings at all.
    has_bindings: bool,
    /// Config warnings surfaced during resolution.
    warnings: Vec<String>,
}

impl HotbarCommandSurface {
    /// Build the command surface from the current app and config state.
    #[must_use]
    pub fn new(app: &App, config: &Config) -> Self {
        let known_action_ids = app
            .hotbar_actions
            .iter()
            .map(|action| action.id())
            .collect::<Vec<_>>();
        let resolution = config.resolve_hotbar_bindings(&known_action_ids);

        let bindings: BTreeMap<u8, codewhale_config::HotbarBinding> = resolution
            .bindings
            .into_iter()
            .map(|binding| (binding.slot, binding))
            .collect();

        let explicitly_disabled = config.hotbar.as_deref().is_some_and(|v| v.is_empty());
        let has_bindings = !bindings.is_empty();
        let warnings: Vec<String> = resolution
            .warnings
            .into_iter()
            .map(|w| w.to_string())
            .collect();

        Self {
            bindings,
            explicitly_disabled,
            has_bindings,
            warnings,
        }
    }

    /// Whether the hotbar panel should be rendered at all.
    #[must_use]
    pub fn panel_enabled(&self) -> bool {
        self.has_bindings
    }

    /// Whether the hotbar is explicitly disabled (empty array in config).
    #[must_use]
    pub fn is_explicitly_disabled(&self) -> bool {
        self.explicitly_disabled
    }

    /// Config warnings (unknown actions, duplicate slots, out-of-range).
    #[must_use]
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    // ── state management ────────────────────────────────────────────

    /// Build the full slot state array (slots 1–8) for sidebar rendering.
    #[must_use]
    pub fn slot_states(&self, app: &App) -> Vec<HotbarSlotState> {
        let alt_prefix = crate::tui::widgets::key_hint::alt_prefix();
        (1..=codewhale_config::HOTBAR_SLOT_COUNT)
            .map(|slot| {
                let Some(binding) = self.bindings.get(&slot) else {
                    return HotbarSlotState {
                        slot,
                        action_id: None,
                        label: "-".to_string(),
                        full_text: format!("{alt_prefix}{slot} · Slot {slot}: empty"),
                        active: false,
                        unknown: false,
                        disabled_reason: None,
                    };
                };

                let Some(action) = app.hotbar_actions.get(&binding.action) else {
                    let label = binding
                        .label
                        .as_deref()
                        .map(str::trim)
                        .filter(|l| !l.is_empty())
                        .map(str::to_string)
                        .unwrap_or_else(|| "?".to_string());
                    return HotbarSlotState {
                        slot,
                        action_id: Some(binding.action.clone()),
                        label,
                        full_text: format!(
                            "{alt_prefix}{slot} · Slot {slot}: unknown action {}",
                            binding.action
                        ),
                        active: false,
                        unknown: true,
                        disabled_reason: None,
                    };
                };

                let label = binding
                    .label
                    .as_deref()
                    .map(str::trim)
                    .filter(|l| !l.is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| action.short_label().to_string());

                let active = action.is_active(app);
                let disabled_reason = action.disabled_reason(app);
                let status = if active { " active" } else { "" };
                HotbarSlotState {
                    slot,
                    action_id: Some(binding.action.clone()),
                    label: label.clone(),
                    full_text: format!(
                        "{alt_prefix}{slot} · Slot {slot}: {label}{status} ({}: {})",
                        action.category(),
                        action.id()
                    ),
                    active,
                    unknown: false,
                    disabled_reason,
                }
            })
            .collect()
    }

    // ── command resolution ──────────────────────────────────────────

    /// Resolve a slot number (1–8) to its bound action id, if any.
    #[must_use]
    pub fn resolve_slot(&self, slot: u8) -> Option<&str> {
        self.bindings.get(&slot).map(|b| b.action.as_str())
    }

    /// Resolve a slot and return the action metadata, if available.
    #[must_use]
    pub fn resolve_slot_metadata(&self, app: &App, slot: u8) -> Option<HotbarActionMetadata> {
        let action_id = self.resolve_slot(slot)?;
        let action = app.hotbar_actions.get(action_id)?;
        Some(action.metadata(app.ui_locale))
    }

    /// Dispatch a hotbar slot through the action registry.
    ///
    /// Returns `None` when the slot is empty. Returns `Some(Handled)`
    /// when the action is unknown or disabled. Otherwise delegates to
    /// the registered action's `dispatch` method.
    pub fn dispatch_slot(&self, app: &mut App, slot: u8) -> anyhow::Result<Option<HotbarDispatch>> {
        let Some(action_id) = self.resolve_slot(slot) else {
            return Ok(None);
        };

        let Some(action) = app.hotbar_actions.get(action_id) else {
            app.status_message = Some(format!(
                "Hotbar slot {slot} action is not available: {action_id}"
            ));
            app.needs_redraw = true;
            return Ok(Some(HotbarDispatch::Handled));
        };

        if let Some(reason) = action.disabled_reason(app) {
            app.status_message = Some(format!(
                "Hotbar slot {slot} action is not available: {reason}"
            ));
            app.needs_redraw = true;
            return Ok(Some(HotbarDispatch::Handled));
        }

        action.dispatch(app).map(Some)
    }

    // ── binding persistence coordination ────────────────────────────

    /// Persist the given bindings to disk and update live config.
    pub fn persist_bindings(
        config_path: Option<&std::path::Path>,
        config: &mut Config,
        bindings: &[codewhale_config::HotbarBindingToml],
    ) -> anyhow::Result<std::path::PathBuf> {
        let path = crate::config_persistence::persist_hotbar_bindings(config_path, bindings)?;
        config.hotbar = Some(bindings.to_vec());
        Ok(path)
    }

    /// Disable the hotbar: persist `hotbar = []` and clear live config.
    pub fn disable(
        config_path: Option<&std::path::Path>,
        config: &mut Config,
    ) -> anyhow::Result<std::path::PathBuf> {
        let path = crate::config_persistence::persist_hotbar_bindings(config_path, &[])?;
        config.hotbar = Some(Vec::new());
        Ok(path)
    }

    /// Restore the default recommended hotbar slots.
    pub fn restore_defaults(
        config_path: Option<&std::path::Path>,
        config: &mut Config,
    ) -> anyhow::Result<std::path::PathBuf> {
        let defaults = codewhale_config::default_hotbar_bindings_toml();
        let path = crate::config_persistence::persist_hotbar_bindings(config_path, &defaults)?;
        config.hotbar = Some(defaults);
        Ok(path)
    }

    // ── recommendation helpers ──────────────────────────────────────

    /// Generate the recommended binding list (default order with labels).
    #[must_use]
    pub fn recommended_bindings(
        app: &App,
        options: HotbarRecommendationOptions,
    ) -> Vec<codewhale_config::HotbarBindingToml> {
        recommended_hotbar_bindings(app, options)
    }

    /// Produce the recommended action entries for display in the setup wizard.
    #[must_use]
    pub fn recommended_entries(
        app: &App,
        options: HotbarRecommendationOptions,
    ) -> Vec<crate::tui::hotbar::actions::HotbarRecommendationEntry> {
        recommend_hotbar_actions(app, options)
    }

    // ── status display integration ──────────────────────────────────

    /// Build a compact status line for the hotbar panel title area.
    #[must_use]
    pub fn status_line(&self) -> String {
        if self.explicitly_disabled {
            "Hotbar: disabled".to_string()
        } else if self.has_bindings {
            let bound = self.bindings.len();
            format!("Hotbar: {bound}/8 slots bound")
        } else {
            "Hotbar: hidden".to_string()
        }
    }

    /// Collect all slot labels for quick comparison in tests.
    #[must_use]
    #[cfg(test)]
    pub fn slot_labels(&self, app: &App) -> Vec<String> {
        self.slot_states(app).into_iter().map(|s| s.label).collect()
    }

    /// Collect all slot action ids for quick comparison in tests.
    #[must_use]
    #[cfg(test)]
    pub fn slot_action_ids(&self) -> Vec<Option<String>> {
        (1..=codewhale_config::HOTBAR_SLOT_COUNT)
            .map(|slot| self.resolve_slot(slot).map(str::to_string))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::config::Config;
    use crate::tui::app::{App, AppMode, TuiOptions};
    use crate::tui::hotbar::actions::HotbarRecommendationOptions;

    use super::*;

    fn test_app() -> App {
        let options = TuiOptions {
            model: "deepseek-v4-pro".to_string(),
            workspace: PathBuf::from("."),
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
            start_in_agent_mode: true,
            skip_onboarding: true,
            yolo: false,
            resume_session_id: None,
            initial_input: None,
        };
        App::new(options, &Config::default())
    }

    // ── state management tests ──────────────────────────────────────

    #[test]
    fn surface_panel_disabled_when_no_config() {
        let app = test_app();
        let surface = HotbarCommandSurface::new(&app, &Config::default());
        assert!(!surface.panel_enabled());
        assert!(!surface.is_explicitly_disabled());
        assert!(surface.warnings().is_empty());
    }

    #[test]
    fn surface_panel_disabled_when_explicitly_empty() {
        let app = test_app();
        let config = Config {
            hotbar: Some(Vec::new()),
            ..Config::default()
        };
        let surface = HotbarCommandSurface::new(&app, &config);
        assert!(!surface.panel_enabled());
        assert!(surface.is_explicitly_disabled());
    }

    #[test]
    fn surface_panel_enabled_with_bindings() {
        let app = test_app();
        let config = Config {
            hotbar: Some(vec![codewhale_config::HotbarBindingToml {
                slot: 1,
                action: "mode.agent".to_string(),
                label: None,
            }]),
            ..Config::default()
        };
        let surface = HotbarCommandSurface::new(&app, &config);
        assert!(surface.panel_enabled());
        assert!(!surface.is_explicitly_disabled());
    }

    #[test]
    fn slot_states_fill_eight_cells() {
        let app = test_app();
        let config = Config {
            hotbar: Some(vec![codewhale_config::HotbarBindingToml {
                slot: 1,
                action: "mode.agent".to_string(),
                label: None,
            }]),
            ..Config::default()
        };
        let surface = HotbarCommandSurface::new(&app, &config);
        let states = surface.slot_states(&app);
        assert_eq!(
            states.len(),
            usize::from(codewhale_config::HOTBAR_SLOT_COUNT)
        );
        assert_eq!(states[0].action_id.as_deref(), Some("mode.agent"));
        assert!(states[0].active);
        assert!(!states[0].unknown);
        assert!(states[0].disabled_reason.is_none());
        // Slot 2 should be empty
        assert_eq!(states[1].action_id, None);
        assert!(!states[1].active);
        assert!(!states[1].unknown);
    }

    #[test]
    fn slot_states_handle_unknown_action() {
        let app = test_app();
        let config = Config {
            hotbar: Some(vec![codewhale_config::HotbarBindingToml {
                slot: 1,
                action: "custom.unknown_action".to_string(),
                label: None,
            }]),
            ..Config::default()
        };
        let surface = HotbarCommandSurface::new(&app, &config);
        let states = surface.slot_states(&app);
        assert!(states[0].unknown);
        assert_eq!(
            states[0].action_id.as_deref(),
            Some("custom.unknown_action")
        );
        assert!(states[0].label == "?");
    }

    #[test]
    fn slot_states_handle_disabled_action() {
        let mut app = test_app();
        app.auto_model = true;
        let config = Config {
            hotbar: Some(vec![codewhale_config::HotbarBindingToml {
                slot: 1,
                action: "reasoning.cycle".to_string(),
                label: None,
            }]),
            ..Config::default()
        };
        let surface = HotbarCommandSurface::new(&app, &config);
        let states = surface.slot_states(&app);
        assert!(!states[0].active);
        assert!(!states[0].unknown);
        assert!(states[0].disabled_reason.is_some());
        assert_eq!(
            states[0].disabled_reason.as_deref(),
            Some("Reasoning effort is controlled by auto model routing.")
        );
    }

    #[test]
    fn slot_states_respect_configured_label() {
        let app = test_app();
        let config = Config {
            hotbar: Some(vec![codewhale_config::HotbarBindingToml {
                slot: 1,
                action: "mode.agent".to_string(),
                label: Some("Agent!".to_string()),
            }]),
            ..Config::default()
        };
        let surface = HotbarCommandSurface::new(&app, &config);
        let states = surface.slot_states(&app);
        assert_eq!(states[0].label, "Agent!");
    }

    #[test]
    fn warnings_captured_from_config_resolution() {
        let app = test_app();
        let config = Config {
            hotbar: Some(vec![
                codewhale_config::HotbarBindingToml {
                    slot: 9,
                    action: "mode.agent".to_string(),
                    label: None,
                },
                codewhale_config::HotbarBindingToml {
                    slot: 1,
                    action: "unknown.fake".to_string(),
                    label: None,
                },
            ]),
            ..Config::default()
        };
        let surface = HotbarCommandSurface::new(&app, &config);
        let warnings = surface.warnings();
        assert!(!warnings.is_empty());
        assert!(warnings.iter().any(|w| w.contains("slot 9")));
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("unknown action 'unknown.fake'"))
        );
    }

    // ── command resolution tests ────────────────────────────────────

    #[test]
    fn resolve_slot_returns_action_id() {
        let app = test_app();
        let config = Config {
            hotbar: Some(vec![codewhale_config::HotbarBindingToml {
                slot: 3,
                action: "mode.yolo".to_string(),
                label: None,
            }]),
            ..Config::default()
        };
        let surface = HotbarCommandSurface::new(&app, &config);
        assert_eq!(surface.resolve_slot(3), Some("mode.yolo"));
        assert_eq!(surface.resolve_slot(1), None);
    }

    #[test]
    fn resolve_slot_metadata_returns_action_metadata() {
        let app = test_app();
        let config = Config {
            hotbar: Some(vec![codewhale_config::HotbarBindingToml {
                slot: 4,
                action: "mode.agent".to_string(),
                label: None,
            }]),
            ..Config::default()
        };
        let surface = HotbarCommandSurface::new(&app, &config);

        let metadata = surface
            .resolve_slot_metadata(&app, 4)
            .expect("slot metadata");

        assert_eq!(metadata.id, "mode.agent");
        assert_eq!(metadata.compact_label, "agent");
        assert!(surface.resolve_slot_metadata(&app, 1).is_none());
    }

    #[test]
    fn dispatch_slot_returns_none_for_empty_slot() {
        let mut app = test_app();
        let surface = HotbarCommandSurface::new(&app, &Config::default());
        let result = surface.dispatch_slot(&mut app, 1).expect("dispatch");
        assert_eq!(result, None);
    }

    #[test]
    fn dispatch_slot_changes_mode() {
        let mut app = test_app();
        let config = Config {
            hotbar: Some(vec![codewhale_config::HotbarBindingToml {
                slot: 1,
                action: "mode.plan".to_string(),
                label: None,
            }]),
            ..Config::default()
        };
        let surface = HotbarCommandSurface::new(&app, &config);
        let result = surface.dispatch_slot(&mut app, 1).expect("dispatch");
        assert_eq!(app.mode, AppMode::Plan);
        assert_eq!(
            result,
            Some(HotbarDispatch::AppAction(
                crate::tui::app::AppAction::ModeChanged(AppMode::Plan)
            ))
        );
    }

    #[test]
    fn dispatch_slot_reports_unknown_action() {
        let mut app = test_app();
        let config = Config {
            hotbar: Some(vec![codewhale_config::HotbarBindingToml {
                slot: 5,
                action: "ghost.action".to_string(),
                label: None,
            }]),
            ..Config::default()
        };
        let surface = HotbarCommandSurface::new(&app, &config);
        let result = surface.dispatch_slot(&mut app, 5).expect("dispatch");
        assert_eq!(result, Some(HotbarDispatch::Handled));
        assert!(
            app.status_message
                .as_deref()
                .is_some_and(|m| m.contains("not available"))
        );
    }

    #[test]
    fn dispatch_slot_reports_disabled_action() {
        let mut app = test_app();
        app.auto_model = true;
        let config = Config {
            hotbar: Some(vec![codewhale_config::HotbarBindingToml {
                slot: 1,
                action: "reasoning.cycle".to_string(),
                label: None,
            }]),
            ..Config::default()
        };
        let surface = HotbarCommandSurface::new(&app, &config);
        let result = surface.dispatch_slot(&mut app, 1).expect("dispatch");
        assert_eq!(result, Some(HotbarDispatch::Handled));
        assert!(
            app.status_message
                .as_deref()
                .is_some_and(|m| m.contains("not available"))
        );
    }

    // ── recommendation tests ────────────────────────────────────────

    #[test]
    fn recommended_bindings_match_default_order() {
        let app = test_app();
        let bindings = HotbarCommandSurface::recommended_bindings(
            &app,
            HotbarRecommendationOptions::default(),
        );
        assert_eq!(
            bindings.len(),
            usize::from(codewhale_config::HOTBAR_SLOT_COUNT)
        );
        assert_eq!(
            bindings
                .iter()
                .map(|b| b.action.as_str())
                .collect::<Vec<_>>(),
            codewhale_config::DEFAULT_HOTBAR_ACTIONS
        );
        for (idx, binding) in bindings.iter().enumerate() {
            assert_eq!(binding.slot as usize, idx + 1);
        }
    }

    #[test]
    fn recommended_entries_exclude_disabled() {
        let mut app = test_app();
        app.auto_model = true;
        let entries =
            HotbarCommandSurface::recommended_entries(&app, HotbarRecommendationOptions::default());
        assert!(!entries.iter().any(|e| e.metadata.id == "reasoning.cycle"));
    }

    // ── status display tests ────────────────────────────────────────

    #[test]
    fn status_line_reports_hidden() {
        let app = test_app();
        let surface = HotbarCommandSurface::new(&app, &Config::default());
        assert_eq!(surface.status_line(), "Hotbar: hidden");
    }

    #[test]
    fn status_line_reports_disabled() {
        let app = test_app();
        let config = Config {
            hotbar: Some(Vec::new()),
            ..Config::default()
        };
        let surface = HotbarCommandSurface::new(&app, &config);
        assert_eq!(surface.status_line(), "Hotbar: disabled");
    }

    #[test]
    fn status_line_reports_bound_count() {
        let app = test_app();
        let config = Config {
            hotbar: Some(vec![
                codewhale_config::HotbarBindingToml {
                    slot: 1,
                    action: "mode.agent".to_string(),
                    label: None,
                },
                codewhale_config::HotbarBindingToml {
                    slot: 2,
                    action: "mode.plan".to_string(),
                    label: None,
                },
            ]),
            ..Config::default()
        };
        let surface = HotbarCommandSurface::new(&app, &config);
        assert_eq!(surface.status_line(), "Hotbar: 2/8 slots bound");
    }

    // ── persistence tests ───────────────────────────────────────────

    #[test]
    fn persist_bindings_updates_config() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let path = tmp.path().join("config.toml");
        let mut config = Config::default();
        let bindings = vec![codewhale_config::HotbarBindingToml {
            slot: 1,
            action: "mode.plan".to_string(),
            label: Some("Plan".to_string()),
        }];

        let written = HotbarCommandSurface::persist_bindings(Some(&path), &mut config, &bindings)
            .expect("persist");

        assert_eq!(written, path);
        assert_eq!(config.hotbar, Some(bindings));
    }

    #[test]
    fn disable_updates_config() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let path = tmp.path().join("config.toml");
        let mut config = Config::default();
        config.hotbar = Some(vec![codewhale_config::HotbarBindingToml {
            slot: 1,
            action: "mode.plan".to_string(),
            label: None,
        }]);

        let written = HotbarCommandSurface::disable(Some(&path), &mut config).expect("disable");

        assert_eq!(written, path);
        assert_eq!(config.hotbar, Some(Vec::new()));
    }

    #[test]
    fn restore_defaults_updates_config() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let path = tmp.path().join("config.toml");
        let mut config = Config::default();

        let written =
            HotbarCommandSurface::restore_defaults(Some(&path), &mut config).expect("restore");

        assert_eq!(written, path);
        assert!(config.hotbar.is_some());
        assert_eq!(
            config.hotbar.unwrap().len(),
            usize::from(codewhale_config::HOTBAR_SLOT_COUNT)
        );
    }

    // ── slot_labels helper tests ────────────────────────────────────

    #[test]
    fn slot_labels_returns_labels_for_all_slots() {
        let app = test_app();
        let config = Config {
            hotbar: Some(vec![codewhale_config::HotbarBindingToml {
                slot: 1,
                action: "mode.agent".to_string(),
                label: None,
            }]),
            ..Config::default()
        };
        let surface = HotbarCommandSurface::new(&app, &config);
        let labels = surface.slot_labels(&app);
        assert_eq!(labels.len(), 8);
        assert_eq!(labels[0], "agent");
        assert_eq!(labels[1], "-");
    }

    #[test]
    fn slot_action_ids_returns_ids_or_none() {
        let app = test_app();
        let config = Config {
            hotbar: Some(vec![codewhale_config::HotbarBindingToml {
                slot: 3,
                action: "mode.yolo".to_string(),
                label: None,
            }]),
            ..Config::default()
        };
        let surface = HotbarCommandSurface::new(&app, &config);
        let ids = surface.slot_action_ids();
        assert_eq!(ids.len(), 8);
        assert_eq!(ids[0], None);
        assert_eq!(ids[2].as_deref(), Some("mode.yolo"));
    }
}
