//! One-shot completion through the `codex` CLI's `exec` subcommand.

use async_trait::async_trait;
use std::process::Stdio;
use tokio::process::Command;

use super::{LocalLLMError, LocalLLMOneShot, Provider};
use crate::util::path::resolve_executable;

const BINARY: &str = "codex";

pub struct CodexOneShot;

impl CodexOneShot {
    pub fn new() -> Self {
        Self
    }
}

impl Default for CodexOneShot {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg_attr(not(target_family = "wasm"), async_trait)]
#[cfg_attr(target_family = "wasm", async_trait(?Send))]
impl LocalLLMOneShot for CodexOneShot {
    async fn complete(
        &self,
        prompt: &str,
        system_prompt: Option<&str>,
    ) -> Result<String, LocalLLMError> {
        if resolve_executable(BINARY).is_none() {
            return Err(LocalLLMError::CliNotInstalled { binary: BINARY });
        }

        // `codex exec` runs non-interactively, prints the assistant's reply to
        // stdout, and exits. `--skip-git-repo-check` keeps it usable from any
        // working directory.
        let mut cmd = Command::new(BINARY);
        cmd.arg("exec")
            .arg("--skip-git-repo-check")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        // Codex doesn't have a dedicated system-prompt flag; prefix it onto
        // the user prompt as a separate paragraph. Callers that need a true
        // system role should use `ClaudeOneShot` (which has --append-system-prompt).
        let combined = match system_prompt {
            Some(sys) if !sys.is_empty() => format!("{sys}\n\n{prompt}"),
            _ => prompt.to_string(),
        };
        cmd.arg(combined);

        let output = cmd.output().await?;
        if !output.status.success() {
            return Err(LocalLLMError::CliFailed {
                binary: BINARY,
                exit_code: output.status.code().unwrap_or(-1),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }

        let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if text.is_empty() {
            return Err(LocalLLMError::EmptyResponse { binary: BINARY });
        }
        Ok(text)
    }

    fn provider(&self) -> Provider {
        Provider::Codex
    }
}
