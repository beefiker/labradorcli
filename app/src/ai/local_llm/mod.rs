//! One-shot LLM completions through the user's locally-installed CLI agents.
//!
//! Used for non-streaming features that historically called Warp's hosted MAA
//! / `/ai/*` endpoints — prompt suggestions, code-review content, block
//! titles. The agent-execution path (full conversational turns) still goes
//! through `agent_sdk/driver` and the `ThirdPartyHarness` trait.

mod claude;
mod codex;

use async_trait::async_trait;
use std::sync::Arc;
use thiserror::Error;

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
    match (codex, claude) {
        (true, true) => Some(default_preference),
        (true, false) => Some(Provider::Codex),
        (false, true) => Some(Provider::Claude),
        (false, false) => None,
    }
}

/// Convenience: build a `LocalLLMOneShot` for the resolved provider.
pub fn build_one_shot(default_preference: Provider) -> Option<Arc<dyn LocalLLMOneShot>> {
    match resolve_provider(default_preference)? {
        Provider::Codex => Some(Arc::new(CodexOneShot::new())),
        Provider::Claude => Some(Arc::new(ClaudeOneShot::new())),
    }
}
