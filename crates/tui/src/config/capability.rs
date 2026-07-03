//! Provider capability matrix.

use super::*;

// ============================================================================
// Provider Capability Matrix
// ============================================================================

/// Known capabilities for a provider + resolved-model combination.
///
/// Returned by [`provider_capability`] to describe what a given provider
/// supports for the resolved model string.  All fields are derived from
/// static knowledge (release docs, API guides) rather than live API probes.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct ProviderCapability {
    /// Canonical provider identifier.
    pub provider: ApiProvider,
    /// Resolved model identifier that will be sent in the API payload.
    pub resolved_model: String,
    /// Context window in tokens (the maximum input the model can accept).
    pub context_window: u32,
    /// Official maximum output tokens for this combo.
    ///
    /// This is model metadata for diagnostics and CI policy. Normal turns use
    /// a separate, more conservative request cap in the engine.
    pub max_output: u32,
    /// Whether the provider+model supports thinking/reasoning mode.
    pub thinking_supported: bool,
    /// Whether the provider returns prompt-cache telemetry fields.
    pub cache_telemetry_supported: bool,
    /// Which request-payload dialect the provider uses.
    pub request_payload_mode: RequestPayloadMode,
    /// Deprecation metadata for compatibility aliases that are still accepted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alias_deprecation: Option<ModelAliasDeprecation>,
}

pub const DEEPSEEK_ALIAS_RETIREMENT_DATE: &str = "2026-07-24";
pub const DEEPSEEK_ALIAS_RETIREMENT_UTC: &str = "2026-07-24T15:59:00Z";
pub const DEEPSEEK_ALIAS_REPLACEMENT: &str = "deepseek-v4-flash";

/// Upstream retirement metadata for a model alias that remains compatible.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct ModelAliasDeprecation {
    pub alias: String,
    pub replacement: String,
    pub retirement_date: String,
    pub retirement_utc: String,
    pub notice: String,
}

/// Which request-payload dialect the provider speaks.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub enum RequestPayloadMode {
    /// Standard OpenAI-compatible `/v1/chat/completions` payload.
    ChatCompletions,
    /// OpenAI Responses API payload.
    Responses,
    /// Native Anthropic Messages API `/v1/messages` payload (#3014).
    AnthropicMessages,
}

/// Resolve the provider capability for a given [`ApiProvider`] and resolved
/// model string.
///
/// The `resolved_model` should be the final model identifier that will appear
/// in the API payload (after normalization / provider-specific mapping).
#[must_use]
pub fn provider_capability(provider: ApiProvider, resolved_model: &str) -> ProviderCapability {
    if matches!(provider, ApiProvider::Anthropic | ApiProvider::Openmodel) {
        return ProviderCapability {
            provider,
            resolved_model: resolved_model.to_string(),
            // 200K is the conservative Anthropic floor; 4.6+ models resolve
            // their 1M windows from models.rs rows (#3014).
            context_window: crate::models::context_window_for_model(resolved_model)
                .unwrap_or(200_000),
            max_output: crate::models::max_output_tokens_for_model(resolved_model)
                .unwrap_or(64_000),
            thinking_supported: crate::models::model_supports_reasoning(resolved_model),
            cache_telemetry_supported: matches!(provider, ApiProvider::Anthropic),
            request_payload_mode: RequestPayloadMode::AnthropicMessages,
            alias_deprecation: None,
        };
    }

    if matches!(provider, ApiProvider::OpenaiCodex) {
        return ProviderCapability {
            provider,
            resolved_model: resolved_model.to_string(),
            context_window: OPENAI_CODEX_EFFECTIVE_CONTEXT_WINDOW_TOKENS,
            max_output: crate::models::max_output_tokens_for_model(resolved_model).unwrap_or(4096),
            thinking_supported: true,
            cache_telemetry_supported: false,
            request_payload_mode: RequestPayloadMode::Responses,
            alias_deprecation: None,
        };
    }

    // #3023: Delete the Openai/Atlascloud/Moonshot early-return so these
    // providers use the generic model-based path below, which correctly
    // resolves context windows, output limits, and thinking support from
    // models.rs lookups.  Ollama also falls through to model-based lookups
    // with 8192 as the last-resort fallback instead of a hardcoded floor.
    if matches!(provider, ApiProvider::XiaomiMimo) {
        return ProviderCapability {
            provider,
            resolved_model: resolved_model.to_string(),
            context_window: crate::models::context_window_for_model(resolved_model)
                .unwrap_or(crate::models::LEGACY_DEEPSEEK_CONTEXT_WINDOW_TOKENS),
            max_output: crate::models::max_output_tokens_for_model(resolved_model).unwrap_or(4096),
            thinking_supported: crate::models::model_supports_reasoning(resolved_model),
            cache_telemetry_supported: false,
            request_payload_mode: RequestPayloadMode::ChatCompletions,
            alias_deprecation: None,
        };
    }

    if matches!(provider, ApiProvider::Arcee) {
        return ProviderCapability {
            provider,
            resolved_model: resolved_model.to_string(),
            context_window: crate::models::context_window_for_model(resolved_model)
                .unwrap_or(crate::models::LEGACY_DEEPSEEK_CONTEXT_WINDOW_TOKENS),
            max_output: crate::models::max_output_tokens_for_model(resolved_model).unwrap_or(4096),
            thinking_supported: crate::models::model_supports_reasoning(resolved_model),
            cache_telemetry_supported: false,
            request_payload_mode: RequestPayloadMode::ChatCompletions,
            alias_deprecation: None,
        };
    }

    let model_lower = resolved_model.to_ascii_lowercase();
    let alias_deprecation = if matches!(
        provider,
        ApiProvider::Deepseek | ApiProvider::DeepseekCN | ApiProvider::DeepseekAnthropic
    ) {
        deepseek_alias_deprecation(&model_lower)
    } else {
        None
    };
    let is_v4_pro = model_lower.contains("v4-pro") || model_lower == "deepseek-v4pro";
    let is_v4_flash = model_lower.contains("v4-flash")
        || model_lower == "deepseek-v4flash"
        || model_lower == "deepseek-v4"
        || alias_deprecation.is_some();
    let is_reasoner = matches!(provider, ApiProvider::WanjieArk)
        && (model_lower.contains("reasoner") || model_lower.contains("r1"));

    // Context window: V4-class models get 1M, everything else falls through
    // to the model's own lookup or a default.  Ollama defaults to 8192
    // (conservative for small local models) instead of 128K.
    let context_window = if is_v4_pro || is_v4_flash {
        crate::models::DEEPSEEK_V4_CONTEXT_WINDOW_TOKENS
    } else if let Some(window) = crate::models::context_window_for_model(resolved_model) {
        window
    } else if matches!(provider, ApiProvider::Ollama) {
        8192
    } else {
        crate::models::LEGACY_DEEPSEEK_CONTEXT_WINDOW_TOKENS
    };

    // Max output tokens: official DeepSeek V4 API metadata lists 384K;
    // runtime request caps remain separate and more conservative.
    let max_output = if is_v4_pro || is_v4_flash {
        384_000
    } else {
        crate::models::max_output_tokens_for_model(resolved_model).unwrap_or(4096)
    };

    // Thinking support: V4 models support thinking on all providers, but
    // only when the model name matches the V4 family.
    let thinking_supported = is_v4_pro
        || is_v4_flash
        || is_reasoner
        || crate::models::model_supports_reasoning(resolved_model);

    // Cache telemetry: returned only by DeepSeek-native and NVIDIA NIM endpoints.
    let cache_telemetry_supported = matches!(
        provider,
        ApiProvider::Deepseek
            | ApiProvider::DeepseekCN
            | ApiProvider::NvidiaNim
            | ApiProvider::Volcengine
    );

    let request_payload_mode = if matches!(
        provider,
        ApiProvider::DeepseekAnthropic | ApiProvider::Openmodel
    ) {
        RequestPayloadMode::AnthropicMessages
    } else {
        RequestPayloadMode::ChatCompletions
    };

    ProviderCapability {
        provider,
        resolved_model: resolved_model.to_string(),
        context_window,
        max_output,
        thinking_supported,
        cache_telemetry_supported,
        request_payload_mode,
        alias_deprecation,
    }
}

fn deepseek_alias_deprecation(model_lower: &str) -> Option<ModelAliasDeprecation> {
    match model_lower {
        "deepseek-chat" | "deepseek-reasoner" => Some(ModelAliasDeprecation {
            alias: model_lower.to_string(),
            replacement: DEEPSEEK_ALIAS_REPLACEMENT.to_string(),
            retirement_date: DEEPSEEK_ALIAS_RETIREMENT_DATE.to_string(),
            retirement_utc: DEEPSEEK_ALIAS_RETIREMENT_UTC.to_string(),
            notice: format!(
                "{model_lower} is a compatibility alias for {DEEPSEEK_ALIAS_REPLACEMENT} and is scheduled to retire on {DEEPSEEK_ALIAS_RETIREMENT_DATE}."
            ),
        }),
        _ => None,
    }
}
