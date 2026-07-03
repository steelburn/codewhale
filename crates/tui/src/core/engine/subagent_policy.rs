//! Sub-agent management policy extracted from `EngineConfig`.
//!
//! All fields were previously direct members of `EngineConfig`; they are
//! now grouped here so the policy surface can be reviewed, tested, and
//! evolved independently of the engine's private wiring (#3942).

use std::collections::HashMap;
use std::time::Duration;

use crate::config::{ApiProvider, Config};
use crate::features::{Feature, Features};
use crate::tui::app::AppMode;

/// Sub-agent concurrency, depth, timeout, and budget controls.
///
/// These values are resolved once at engine construction from the
/// `[subagents]` config section and the active provider's limits,
/// then threaded through `SubAgentRuntime` and `SharedSubAgentManager`
/// without further config lookups.
#[derive(Debug, Clone)]
pub struct SubagentPolicy {
    /// Maximum number of concurrently active subagents.
    pub max_subagents: usize,
    /// Maximum queued + running sub-agents admitted for this engine session.
    pub max_admitted_subagents: usize,
    /// Number of direct (depth-1) sub-agents that may execute concurrently
    /// before further launches queue for a launch slot (#3095).
    /// Resolved from `[subagents] launch_concurrency`.
    pub launch_concurrency: usize,
    /// Whether the model-facing `agent` tool is available after applying
    /// feature flags and `[subagents]` opt-out controls.
    pub enabled: bool,
    /// Maximum sub-agent recursion depth (default 3). See
    /// `SubAgentRuntime::max_spawn_depth`. Override via
    /// `[subagents] max_depth = N` in `~/.codewhale/config.toml`.
    pub max_spawn_depth: u32,
    /// Optional aggregate token budget for each root sub-agent run.
    /// Descendant agents inherit the root pool unless a child starts a new
    /// budget scope with an explicit per-call override.
    pub token_budget: Option<u64>,
    /// Per-role/type sub-agent model overrides already resolved from config.
    pub model_overrides: HashMap<String, String>,
    /// Per-step DeepSeek API timeout for sub-agent requests (#1806, #1808).
    pub api_timeout: Duration,
    /// Wall-clock interval without manager-visible sub-agent progress
    /// before a running child can be auto-cancelled to release its slot
    /// (#2614).
    pub heartbeat_timeout: Duration,
}

impl Default for SubagentPolicy {
    fn default() -> Self {
        Self {
            max_subagents: crate::config::DEFAULT_MAX_SUBAGENTS,
            max_admitted_subagents: crate::config::DEFAULT_MAX_SUBAGENTS,
            launch_concurrency: crate::config::DEFAULT_MAX_SUBAGENTS,
            enabled: true,
            max_spawn_depth: crate::tools::subagent::DEFAULT_MAX_SPAWN_DEPTH,
            token_budget: None,
            model_overrides: HashMap::new(),
            api_timeout: Duration::from_secs(crate::config::DEFAULT_SUBAGENT_API_TIMEOUT_SECS),
            heartbeat_timeout: Duration::from_secs(
                crate::config::DEFAULT_SUBAGENT_HEARTBEAT_TIMEOUT_SECS,
            ),
        }
    }
}

impl SubagentPolicy {
    /// Resolve the complete sub-agent launch policy from config for one runtime
    /// route.
    ///
    /// `max_subagents` is passed in because CLI/TUI callers may already have
    /// applied an explicit runtime override before policy construction.
    pub fn from_config_for_provider(
        config: &Config,
        provider: ApiProvider,
        max_subagents: usize,
    ) -> Self {
        let max_subagents = max_subagents.clamp(1, crate::config::MAX_SUBAGENTS);
        Self {
            max_subagents,
            max_admitted_subagents: config
                .max_admitted_subagents_for_provider(provider)
                .max(max_subagents),
            launch_concurrency: config
                .launch_concurrency_for_provider(provider)
                .clamp(1, max_subagents),
            enabled: config.subagents_enabled_for_provider(provider),
            max_spawn_depth: config.subagent_max_spawn_depth_for_provider(provider),
            token_budget: config.subagent_token_budget_for_provider(provider),
            model_overrides: config.subagent_model_overrides(),
            api_timeout: Duration::from_secs(
                config.subagent_api_timeout_secs_for_provider(provider),
            ),
            heartbeat_timeout: Duration::from_secs(
                config.subagent_heartbeat_timeout_secs_for_provider(provider),
            ),
        }
    }

    /// Whether this turn may expose the model-facing `agent` launcher.
    pub fn exposes_agent_tool(&self, features: &Features, mode: AppMode) -> bool {
        self.enabled
            && features.enabled(Feature::Subagents)
            && matches!(mode, AppMode::Agent | AppMode::Auto | AppMode::Yolo)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_tool_exposure_requires_policy_feature_and_runtime_mode() {
        let features = Features::with_defaults();
        let policy = SubagentPolicy::default();

        assert!(policy.exposes_agent_tool(&features, AppMode::Agent));
        assert!(policy.exposes_agent_tool(&features, AppMode::Auto));
        assert!(policy.exposes_agent_tool(&features, AppMode::Yolo));
        assert!(!policy.exposes_agent_tool(&features, AppMode::Plan));

        let mut disabled_policy = policy.clone();
        disabled_policy.enabled = false;
        assert!(!disabled_policy.exposes_agent_tool(&features, AppMode::Agent));

        let mut disabled_feature = features.clone();
        disabled_feature.disable(Feature::Subagents);
        assert!(!policy.exposes_agent_tool(&disabled_feature, AppMode::Agent));
    }

    #[test]
    fn config_resolution_clamps_launch_concurrency_to_runtime_cap() {
        let mut config = Config::default();
        config.subagents = Some(crate::config::SubagentsConfig {
            launch_concurrency: Some(8),
            ..Default::default()
        });

        let policy = SubagentPolicy::from_config_for_provider(&config, ApiProvider::Deepseek, 2);

        assert_eq!(policy.max_subagents, 2);
        assert_eq!(policy.launch_concurrency, 2);
        assert!(policy.max_admitted_subagents >= policy.max_subagents);
        assert!(policy.enabled);
    }
}
