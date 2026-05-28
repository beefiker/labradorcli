//! One-shot LLM completions through the user's locally-installed CLI agents.
//!
//! Used for non-streaming features that historically called Labrador's hosted MAA
//! / `/ai/*` endpoints — prompt suggestions, code-review content, block
//! titles. The agent-execution path (full conversational turns) still goes
//! through `agent_sdk/driver` and the `ThirdPartyHarness` trait.

mod claude;
mod codex;
pub mod suggestion;

use async_trait::async_trait;
use std::sync::Arc;
use thiserror::Error;

use ai::{local_claude_auth, local_openai_auth};

use crate::util::path::resolve_executable;

pub use claude::ClaudeOneShot;
pub use codex::CodexOneShot;

/// LLM provider that can be driven through a locally-installed CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Codex,
    Claude,
}

impl Provider {
    /// Name of the executable on `PATH`.
    pub fn binary_name(self) -> &'static str {
        match self {
            Provider::Codex => "codex",
            Provider::Claude => "claude",
        }
    }

    /// Whether the user has the CLI installed (resolves on `PATH`).
    pub fn is_installed(self) -> bool {
        resolve_executable(self.binary_name()).is_some()
    }
}

/// One-shot text completion through a local CLI. No streaming, no agent loop.
///
/// Implementations shell out to the provider's CLI in non-interactive mode,
/// capture stdout, and return the model's response as a single string. Use
/// for features like prompt suggestion, code review content generation, and
/// block title generation — anywhere the previous code POSTed to a hosted
/// endpoint that took a prompt and returned text.
#[cfg_attr(not(target_family = "wasm"), async_trait)]
#[cfg_attr(target_family = "wasm", async_trait(?Send))]
pub trait LocalLLMOneShot: Send + Sync {
    /// Send `prompt` (with optional `system_prompt`) and return the model's
    /// text response.
    async fn complete(
        &self,
        prompt: &str,
        system_prompt: Option<&str>,
    ) -> Result<String, LocalLLMError>;

    /// Provider this implementation talks to. For telemetry/debugging.
    #[allow(dead_code)]
    fn provider(&self) -> Provider;
}

#[derive(Debug, Error)]
pub enum LocalLLMError {
    #[error("`{binary}` CLI not found on PATH")]
    CliNotInstalled { binary: &'static str },

    #[error("`{binary}` exited with code {exit_code}: {stderr}")]
    CliFailed {
        binary: &'static str,
        exit_code: i32,
        stderr: String,
    },

    #[error("`{binary}` produced an empty response")]
    EmptyResponse { binary: &'static str },

    #[error("io error invoking local CLI: {0}")]
    Io(#[from] std::io::Error),
}

/// Resolves which provider to use for a one-shot request.
///
/// Decision order:
/// 1. If only one of {Codex, Claude} is installed, use that one.
/// 2. If both are installed, use `default_preference`.
/// 3. If neither is installed, return `None`.
pub fn resolve_provider(default_preference: Provider) -> Option<Provider> {
    let codex = Provider::Codex.is_installed();
    let claude = Provider::Claude.is_installed();
    let codex_authed = local_openai_auth::has_access_token();
    let claude_authed = local_claude_auth::has_auth_state();

    resolve_provider_from_state(
        default_preference,
        codex,
        claude,
        codex_authed,
        claude_authed,
    )
}

fn resolve_provider_from_state(
    default_preference: Provider,
    codex_installed: bool,
    claude_installed: bool,
    codex_authed: bool,
    claude_authed: bool,
) -> Option<Provider> {
    match (codex_installed, claude_installed) {
        (true, true) => Some(default_preference),
        (true, false) => Some(Provider::Codex),
        (false, true) => Some(Provider::Claude),
        (false, false) => None,
    }
    .and_then(|provider| match provider {
        Provider::Codex if !codex_authed && claude_installed && claude_authed => Some(Provider::Claude),
        Provider::Claude if !claude_authed && codex_installed && codex_authed => Some(Provider::Codex),
        p => Some(p),
    })
}

#[cfg(test)]
mod tests {
    use super::{Provider, resolve_provider_from_state};

    #[test]
    fn prefers_authenticated_provider_when_default_is_unauthenticated() {
        let resolved = resolve_provider_from_state(Provider::Codex, true, true, false, true);
        assert_eq!(resolved, Some(Provider::Claude));
    }

    #[test]
    fn keeps_default_when_both_providers_are_authenticated() {
        let resolved = resolve_provider_from_state(Provider::Codex, true, true, true, true);
        assert_eq!(resolved, Some(Provider::Codex));
    }

    #[test]
    fn falls_back_to_installed_provider_even_if_not_authenticated() {
        let resolved = resolve_provider_from_state(Provider::Codex, true, false, false, false);
        assert_eq!(resolved, Some(Provider::Codex));
    }
}

/// Convenience: build a `LocalLLMOneShot` for the resolved provider.
pub fn build_one_shot(default_preference: Provider) -> Option<Arc<dyn LocalLLMOneShot>> {
    match resolve_provider(default_preference)? {
        Provider::Codex => Some(Arc::new(CodexOneShot::new())),
        Provider::Claude => Some(Arc::new(ClaudeOneShot::new())),
    }
}
