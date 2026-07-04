//! Prompt override storage, opt-in gates, and effective value resolution.

use super::{
    AUTHORITY_RECAP, BASE_PROMPT, LOCALE_CLOSER_JA, LOCALE_CLOSER_PT_BR, LOCALE_CLOSER_VI,
    LOCALE_CLOSER_ZH_HANS, LOCALE_PREAMBLE_JA, LOCALE_PREAMBLE_PT_BR, LOCALE_PREAMBLE_VI,
    LOCALE_PREAMBLE_ZH_HANS, Personality,
};
use std::path::Path;
use std::sync::{LazyLock, Mutex, OnceLock};

// ── Embedder prompt overrides ──
// Let an embedder replace these compile-time prompt constants at startup,
// so brand / slimming customizations live in the embedder crate instead of
// editing these files in-tree. Unset → the bundled constant (fully
// backward compatible). Intended to be set once at process start, before
// any engine spawns; later sets return the rejected override string.
static BASE_PROMPT_OVERRIDE: OnceLock<String> = OnceLock::new();
static LOCALE_PREAMBLE_ZH_HANS_OVERRIDE: OnceLock<String> = OnceLock::new();
static LOCALE_PREAMBLE_JA_OVERRIDE: OnceLock<String> = OnceLock::new();
static LOCALE_PREAMBLE_PT_BR_OVERRIDE: OnceLock<String> = OnceLock::new();
static LOCALE_PREAMBLE_VI_OVERRIDE: OnceLock<String> = OnceLock::new();
static LOCALE_CLOSER_ZH_HANS_OVERRIDE: OnceLock<String> = OnceLock::new();
static LOCALE_CLOSER_JA_OVERRIDE: OnceLock<String> = OnceLock::new();
static LOCALE_CLOSER_PT_BR_OVERRIDE: OnceLock<String> = OnceLock::new();
static LOCALE_CLOSER_VI_OVERRIDE: OnceLock<String> = OnceLock::new();
static AUTHORITY_RECAP_OVERRIDE: OnceLock<String> = OnceLock::new();
static STATIC_PROMPT_COMPOSER: OnceLock<Box<StaticPromptComposer>> = OnceLock::new();
static PROMPT_OVERRIDE_NOTICES: LazyLock<Mutex<Vec<String>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));

/// Context passed to an embedder-provided static prompt composer.
///
/// This hook only replaces the byte-stable base/personality prompt segment.
/// Mode deltas, approval policy, tool taxonomy, Context Management, and the
/// Compaction Relay stay owned by CodeWhale's system prompt assembly.
#[non_exhaustive]
#[derive(Debug)]
pub struct StaticPromptCtx<'a> {
    /// Active model identifier after caller-side routing.
    pub model_id: &'a str,
    /// Personality overlay requested for the base static prompt.
    pub personality: Personality,
    /// Default base/personality prompt layers that would be used without an
    /// override.
    pub default_layers: &'a str,
}

/// Embedder hook for replacing CodeWhale's byte-stable base/personality prompt
/// segment.
pub type StaticPromptComposer = dyn Fn(&StaticPromptCtx<'_>) -> String + Send + Sync + 'static;

/// Replace `BASE_PROMPT` for all subsequent prompt composition. First call
/// wins; later calls return the rejected string. Set before spawning any
/// engine.
pub fn set_base_prompt_override(s: String) -> Result<(), String> {
    set_prompt_override(&BASE_PROMPT_OVERRIDE, s)
}

/// Replace the Simplified-Chinese locale preamble (`## 语言要求`).
pub fn set_locale_preamble_zh_hans_override(s: String) -> Result<(), String> {
    set_prompt_override(&LOCALE_PREAMBLE_ZH_HANS_OVERRIDE, s)
}

/// Replace the Japanese locale preamble.
pub fn set_locale_preamble_ja_override(s: String) -> Result<(), String> {
    set_prompt_override(&LOCALE_PREAMBLE_JA_OVERRIDE, s)
}

/// Replace the Brazilian-Portuguese locale preamble.
pub fn set_locale_preamble_pt_br_override(s: String) -> Result<(), String> {
    set_prompt_override(&LOCALE_PREAMBLE_PT_BR_OVERRIDE, s)
}

/// Replace the Vietnamese locale preamble.
pub fn set_locale_preamble_vi_override(s: String) -> Result<(), String> {
    set_prompt_override(&LOCALE_PREAMBLE_VI_OVERRIDE, s)
}

/// Replace the Simplified-Chinese locale closer (`## 语言再次提醒`).
pub fn set_locale_closer_zh_hans_override(s: String) -> Result<(), String> {
    set_prompt_override(&LOCALE_CLOSER_ZH_HANS_OVERRIDE, s)
}

/// Replace the Japanese locale closer.
pub fn set_locale_closer_ja_override(s: String) -> Result<(), String> {
    set_prompt_override(&LOCALE_CLOSER_JA_OVERRIDE, s)
}

/// Replace the Brazilian-Portuguese locale closer.
pub fn set_locale_closer_pt_br_override(s: String) -> Result<(), String> {
    set_prompt_override(&LOCALE_CLOSER_PT_BR_OVERRIDE, s)
}

/// Replace the Vietnamese locale closer.
pub fn set_locale_closer_vi_override(s: String) -> Result<(), String> {
    set_prompt_override(&LOCALE_CLOSER_VI_OVERRIDE, s)
}

/// Replace the trailing `## Authority Recap` block.
pub fn set_authority_recap_override(s: String) -> Result<(), String> {
    set_prompt_override(&AUTHORITY_RECAP_OVERRIDE, s)
}

/// Replace the byte-stable base/personality prompt segment for subsequent
/// prompt composition. First call wins; later calls return the rejected
/// composer so embedders can preserve ownership.
pub fn set_static_prompt_composer_override(
    f: Box<StaticPromptComposer>,
) -> Result<(), Box<StaticPromptComposer>> {
    set_static_prompt_composer(&STATIC_PROMPT_COMPOSER, f)
}

// ── Config-directory prompt overrides (issue #3638) ──
// Bridge the embedder override hooks above to a user-facing source: an
// optional file in the CodeWhale config directory. This lets users repurpose
// the TUI for non-software use cases (e.g. long-form writing) by swapping the
// constitutional base prompt, without editing in-tree files or shipping a
// custom embedder build.
//
// Scope is deliberately narrow: only the byte-stable base prompt segment is
// user-overridable. Mode deltas, approval policy, tool taxonomy, Context
// Management, and the Compaction Relay stay owned by the runtime assembly (see
// `StaticPromptCtx`), so an override cannot strip safety-relevant guidance.
// A missing or empty file is a no-op — the bundled constant is used — so this
// is fully backward compatible.
//
// Because replacing the base prompt is a trust-boundary action (per maintainer
// review on #3638), the override file alone is NOT sufficient: the user must
// also set an explicit opt-in flag (`CODEWHALE_ALLOW_BASE_PROMPT_OVERRIDE`).
// This keeps replacing the global Constitution a deliberate, auditable act
// rather than something a stray file can do.

/// Relative path, under the config directory, of the optional base-prompt
/// (constitution) override file.
pub const CONSTITUTION_OVERRIDE_FILE: &str = "prompts/constitution.md";

/// Env flag that must be set (`1`/`true`/`on`/`yes`) to enable config-dir base
/// prompt overrides. Required in addition to the override file so the global
/// base prompt can never be replaced by file presence alone.
pub const BASE_PROMPT_OVERRIDE_OPT_IN_ENV: &str = "CODEWHALE_ALLOW_BASE_PROMPT_OVERRIDE";

/// Whether the user has explicitly opted in to base-prompt overrides.
fn base_prompt_override_opt_in() -> bool {
    match std::env::var(BASE_PROMPT_OVERRIDE_OPT_IN_ENV) {
        Ok(v) => matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "on" | "yes"
        ),
        Err(_) => false,
    }
}

/// Read an optional prompt-override file rooted at `config_dir`.
///
/// Returns the file contents when it exists and is non-empty after trimming;
/// otherwise `None` so the caller falls back to the embedded default. Pure
/// over `config_dir`, so it is unit-testable without touching the global
/// override cells.
pub(super) fn read_prompt_override_file(config_dir: &Path, relative: &str) -> Option<String> {
    let path = config_dir.join(relative);
    let raw = std::fs::read_to_string(&path).ok()?;
    if raw.trim().is_empty() {
        tracing::warn!(
            target: "prompts",
            "ignoring empty prompt override file {}",
            path.display(),
        );
        return None;
    }
    tracing::info!(
        target: "prompts",
        "loaded prompt override from {}",
        path.display(),
    );
    Some(raw)
}

fn push_prompt_override_notice(message: String) {
    if let Ok(mut notices) = PROMPT_OVERRIDE_NOTICES.lock() {
        notices.push(message);
    }
}

pub fn take_prompt_override_notices() -> Vec<String> {
    PROMPT_OVERRIDE_NOTICES
        .lock()
        .map(|mut notices| std::mem::take(&mut *notices))
        .unwrap_or_default()
}

/// Load user prompt overrides from `config_dir` and install them through the
/// existing override hooks. Returns the names of the overrides that were
/// applied (for logging/diagnostics).
///
/// Call once at startup, before any engine spawns, because the underlying
/// override cells are first-call-wins. Missing files are a no-op, preserving
/// the bundled defaults.
pub fn load_config_dir_prompt_overrides(config_dir: &Path) -> Vec<&'static str> {
    let mut applied = Vec::new();
    if let Some(text) = read_prompt_override_file(config_dir, CONSTITUTION_OVERRIDE_FILE) {
        if !base_prompt_override_opt_in() {
            // A file exists but the user hasn't opted in. Don't silently
            // replace the base prompt — surface the gate instead.
            let warning = format!(
                "Custom Constitution override found at {}/{} but {} is not set; using the bundled Constitution. Set {}=1 to opt in.",
                config_dir.display(),
                CONSTITUTION_OVERRIDE_FILE,
                BASE_PROMPT_OVERRIDE_OPT_IN_ENV,
                BASE_PROMPT_OVERRIDE_OPT_IN_ENV,
            );
            tracing::warn!(
                target: "prompts",
                "{warning}",
            );
            push_prompt_override_notice(warning);
        } else if set_base_prompt_override(text).is_ok() {
            applied.push("constitution");
        }
    }
    applied
}

/// Resolve the CodeWhale config directory and load any prompt overrides found
/// there. Convenience wrapper around [`load_config_dir_prompt_overrides`] for
/// startup wiring; silently does nothing when the config home cannot be
/// resolved.
pub fn load_prompt_overrides_from_config_home() {
    let Ok(home) = codewhale_config::codewhale_home() else {
        return;
    };
    let applied = load_config_dir_prompt_overrides(&home);
    if !applied.is_empty() {
        tracing::info!(
            target: "prompts",
            "applied {} config-directory prompt override(s): {}",
            applied.len(),
            applied.join(", "),
        );
    }
}

pub(super) fn set_prompt_override(cell: &OnceLock<String>, s: String) -> Result<(), String> {
    cell.set(s)
}

pub(super) fn set_static_prompt_composer(
    cell: &OnceLock<Box<StaticPromptComposer>>,
    f: Box<StaticPromptComposer>,
) -> Result<(), Box<StaticPromptComposer>> {
    cell.set(f)
}

pub(super) fn effective_prompt_override<'a>(
    cell: &'a OnceLock<String>,
    fallback: &'static str,
) -> &'a str {
    cell.get().map(String::as_str).unwrap_or(fallback)
}

pub(super) fn effective_base_prompt() -> &'static str {
    effective_prompt_override(&BASE_PROMPT_OVERRIDE, BASE_PROMPT)
}

pub(super) fn effective_static_prompt_composer() -> Option<&'static StaticPromptComposer> {
    STATIC_PROMPT_COMPOSER.get().map(Box::as_ref)
}

pub(super) fn effective_locale_preamble_zh_hans() -> &'static str {
    effective_prompt_override(&LOCALE_PREAMBLE_ZH_HANS_OVERRIDE, LOCALE_PREAMBLE_ZH_HANS)
}

pub(super) fn effective_locale_preamble_ja() -> &'static str {
    effective_prompt_override(&LOCALE_PREAMBLE_JA_OVERRIDE, LOCALE_PREAMBLE_JA)
}

pub(super) fn effective_locale_preamble_pt_br() -> &'static str {
    effective_prompt_override(&LOCALE_PREAMBLE_PT_BR_OVERRIDE, LOCALE_PREAMBLE_PT_BR)
}

pub(super) fn effective_locale_preamble_vi() -> &'static str {
    effective_prompt_override(&LOCALE_PREAMBLE_VI_OVERRIDE, LOCALE_PREAMBLE_VI)
}

pub(super) fn effective_locale_closer_zh_hans() -> &'static str {
    effective_prompt_override(&LOCALE_CLOSER_ZH_HANS_OVERRIDE, LOCALE_CLOSER_ZH_HANS)
}

pub(super) fn effective_locale_closer_ja() -> &'static str {
    effective_prompt_override(&LOCALE_CLOSER_JA_OVERRIDE, LOCALE_CLOSER_JA)
}

pub(super) fn effective_locale_closer_pt_br() -> &'static str {
    effective_prompt_override(&LOCALE_CLOSER_PT_BR_OVERRIDE, LOCALE_CLOSER_PT_BR)
}

pub(super) fn effective_locale_closer_vi() -> &'static str {
    effective_prompt_override(&LOCALE_CLOSER_VI_OVERRIDE, LOCALE_CLOSER_VI)
}

pub(super) fn effective_authority_recap() -> &'static str {
    effective_prompt_override(&AUTHORITY_RECAP_OVERRIDE, AUTHORITY_RECAP)
}
