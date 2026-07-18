use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use crate::ProviderKind;

/// Schema version for informed consent to another CLI's credential file.
pub const EXTERNAL_CREDENTIAL_CONSENT_VERSION: u32 = 1;

/// Resolve a user-selected path without touching the filesystem.
///
/// Consent is bound to the exact logical path, so this intentionally avoids
/// canonicalization (which would stat the candidate before consent exists).
pub fn resolve_external_credential_path(path: impl AsRef<Path>) -> Result<PathBuf> {
    let path = path.as_ref();
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    Ok(std::env::current_dir()
        .map_err(|err| anyhow::anyhow!("resolving external credential path: {err}"))?
        .join(path))
}

/// The side-effect envelope Codewhale may use for an external credential.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalCredentialAccess {
    /// Do not inspect or access the external credential store.
    #[default]
    Disabled,
    /// Read the exact selected file without refreshing or rewriting it.
    ReadOnly,
    /// Permit a documented preservation adapter to refresh and rewrite it.
    Managed,
}

impl ExternalCredentialAccess {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::ReadOnly => "read_only",
            Self::Managed => "managed",
        }
    }
}

/// External credential owners supported by the consent schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalCredentialSource {
    CodexCli,
    KimiCodeCli,
    GrokCli,
}

impl ExternalCredentialSource {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CodexCli => "codex_cli",
            Self::KimiCodeCli => "kimi_code_cli",
            Self::GrokCli => "grok_cli",
        }
    }
}

/// Persisted, provider-scoped consent for one exact external credential file.
///
/// Provider and source are repeated intentionally. A copied provider table or
/// a future source-path remap must fail closed instead of inheriting authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalCredentialConsentToml {
    pub access: ExternalCredentialAccess,
    pub provider: String,
    pub source: ExternalCredentialSource,
    pub path: PathBuf,
    pub consent_version: u32,
}

impl ExternalCredentialConsentToml {
    #[must_use]
    pub fn read_only(
        provider: ProviderKind,
        source: ExternalCredentialSource,
        path: PathBuf,
    ) -> Self {
        Self {
            access: ExternalCredentialAccess::ReadOnly,
            provider: provider.as_str().to_string(),
            source,
            path,
            consent_version: EXTERNAL_CREDENTIAL_CONSENT_VERSION,
        }
    }

    /// Validate that this record is a current read-only consent for one exact
    /// provider/source/path tuple without minting an I/O capability.
    ///
    /// This is intentionally side-effect free so inventory and picker surfaces
    /// can acknowledge dormant consent without inspecting the external file.
    pub fn validate_read_scope(
        &self,
        provider: ProviderKind,
        source: ExternalCredentialSource,
        resolved_path: &Path,
    ) -> Result<()> {
        if self.access == ExternalCredentialAccess::Disabled {
            bail!(
                "external credential access is disabled for {}",
                provider.as_str()
            );
        }
        if self.access == ExternalCredentialAccess::Managed {
            bail!(
                "managed external credential access is unsupported for {}; no schema-safe preservation adapter is available",
                provider.as_str()
            );
        }
        if self.consent_version != EXTERNAL_CREDENTIAL_CONSENT_VERSION {
            bail!(
                "external credential consent for {} uses unsupported version {}; revoke and consent again",
                provider.as_str(),
                self.consent_version
            );
        }
        if self.provider != provider.as_str() {
            bail!(
                "external credential consent is scoped to provider {}, not {}",
                self.provider,
                provider.as_str()
            );
        }
        if self.source != source {
            bail!(
                "external credential consent source mismatch for {} (expected {})",
                provider.as_str(),
                source.as_str()
            );
        }
        if !self.path.is_absolute() {
            bail!(
                "external credential consent path for {} must be absolute",
                provider.as_str()
            );
        }
        if self.path != resolved_path {
            bail!(
                "external credential path changed for {}; consent covers {}, current path is {}",
                provider.as_str(),
                self.path.display(),
                resolved_path.display()
            );
        }
        Ok(())
    }

    /// Validate and mint the read capability consumed by credential adapters.
    /// No filesystem operation occurs while validating the policy.
    pub fn read_grant(
        &self,
        provider: ProviderKind,
        source: ExternalCredentialSource,
        resolved_path: &Path,
    ) -> Result<ExternalCredentialReadGrant> {
        self.validate_read_scope(provider, source, resolved_path)?;
        Ok(ExternalCredentialReadGrant {
            provider,
            source,
            path: resolved_path.to_path_buf(),
            consent_version: self.consent_version,
        })
    }
}

/// Opaque proof that one exact provider/source/path tuple may be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalCredentialReadGrant {
    provider: ProviderKind,
    source: ExternalCredentialSource,
    path: PathBuf,
    consent_version: u32,
}

impl ExternalCredentialReadGrant {
    #[must_use]
    pub fn provider(&self) -> ProviderKind {
        self.provider
    }

    #[must_use]
    pub fn source(&self) -> ExternalCredentialSource {
        self.source
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub fn consent_version(&self) -> u32 {
        self.consent_version
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn absolute_test_path(file: &str) -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(format!(r"C:\Users\test\{file}"))
        } else {
            PathBuf::from(format!("/tmp/{file}"))
        }
    }

    #[test]
    fn read_grant_requires_exact_provider_source_path_and_version() {
        let path = absolute_test_path("codex-auth.json");
        let consent = ExternalCredentialConsentToml::read_only(
            ProviderKind::OpenaiCodex,
            ExternalCredentialSource::CodexCli,
            path.clone(),
        );

        let grant = consent
            .read_grant(
                ProviderKind::OpenaiCodex,
                ExternalCredentialSource::CodexCli,
                &path,
            )
            .expect("exact consent tuple");
        assert_eq!(grant.path(), path);

        assert!(
            consent
                .read_grant(ProviderKind::Xai, ExternalCredentialSource::CodexCli, &path)
                .is_err()
        );
        assert!(
            consent
                .read_grant(
                    ProviderKind::OpenaiCodex,
                    ExternalCredentialSource::GrokCli,
                    &path
                )
                .is_err()
        );
        assert!(
            consent
                .read_grant(
                    ProviderKind::OpenaiCodex,
                    ExternalCredentialSource::CodexCli,
                    &path.with_file_name("other.json")
                )
                .is_err()
        );
    }

    #[test]
    fn managed_consent_is_explicitly_unsupported_without_an_adapter() {
        let path = absolute_test_path("grok-auth.json");
        let mut consent = ExternalCredentialConsentToml::read_only(
            ProviderKind::Xai,
            ExternalCredentialSource::GrokCli,
            path.clone(),
        );
        consent.access = ExternalCredentialAccess::Managed;

        let error = consent
            .read_grant(ProviderKind::Xai, ExternalCredentialSource::GrokCli, &path)
            .expect_err("managed access must fail closed");
        assert!(
            error
                .to_string()
                .contains("schema-safe preservation adapter")
        );
    }

    #[test]
    fn consent_round_trips_every_scope_field() {
        let path = absolute_test_path("codex-auth.json");
        let consent = ExternalCredentialConsentToml::read_only(
            ProviderKind::OpenaiCodex,
            ExternalCredentialSource::CodexCli,
            path,
        );

        let encoded = toml::to_string(&consent).expect("serialize consent");
        let decoded: ExternalCredentialConsentToml =
            toml::from_str(&encoded).expect("deserialize consent");
        assert_eq!(decoded, consent);
        assert!(encoded.contains("access = \"read_only\""));
        assert!(encoded.contains("provider = \"openai-codex\""));
        assert!(encoded.contains("source = \"codex_cli\""));
        assert!(encoded.contains("consent_version = 1"));
    }

    #[test]
    fn disabled_stale_and_relative_consent_fail_before_a_grant() {
        let path = absolute_test_path("grok-auth.json");
        let mut consent = ExternalCredentialConsentToml::read_only(
            ProviderKind::Xai,
            ExternalCredentialSource::GrokCli,
            path.clone(),
        );

        consent.access = ExternalCredentialAccess::Disabled;
        assert!(
            consent
                .read_grant(ProviderKind::Xai, ExternalCredentialSource::GrokCli, &path)
                .expect_err("disabled consent")
                .to_string()
                .contains("disabled")
        );

        consent.access = ExternalCredentialAccess::ReadOnly;
        consent.consent_version = EXTERNAL_CREDENTIAL_CONSENT_VERSION + 1;
        assert!(
            consent
                .read_grant(ProviderKind::Xai, ExternalCredentialSource::GrokCli, &path)
                .expect_err("stale consent")
                .to_string()
                .contains("unsupported version")
        );

        consent.consent_version = EXTERNAL_CREDENTIAL_CONSENT_VERSION;
        consent.path = PathBuf::from("relative/auth.json");
        assert!(
            consent
                .read_grant(
                    ProviderKind::Xai,
                    ExternalCredentialSource::GrokCli,
                    Path::new("relative/auth.json"),
                )
                .expect_err("relative path")
                .to_string()
                .contains("must be absolute")
        );
    }
}
