//! One-shot completion through the `claude` CLI in print mode.

use async_trait::async_trait;
use std::process::Stdio;
use tokio::process::Command;

use super::{LocalLLMError, LocalLLMOneShot, Provider};
use crate::util::path::resolve_executable;

const BINARY: &str = "claude";

pub struct ClaudeOneShot;

impl ClaudeOneShot {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ClaudeOneShot {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg_attr(not(target_family = "wasm"), async_trait)]
#[cfg_attr(target_family = "wasm", async_trait(?Send))]
impl LocalLLMOneShot for ClaudeOneShot {
    async fn complete(
        &self,
        prompt: &str,
        system_prompt: Option<&str>,
    ) -> Result<String, LocalLLMError> {
        if resolve_executable(BINARY).is_none() {
            return Err(LocalLLMError::CliNotInstalled { binary: BINARY });
        }

        // `claude -p <prompt>` is print mode: non-interactive, prints the
        // assistant's reply to stdout, exits.
        let mut cmd = Command::new(BINARY);
        cmd.arg("-p");
        if let Some(sys) = system_prompt.filter(|s| !s.is_empty()) {
            cmd.arg("--append-system-prompt").arg(sys);
        }
        cmd.arg(prompt);
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

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
        Provider::Claude
    }
}
