use std::{env, fs, path::PathBuf};

use anyhow::{anyhow, Context as _, Result};
use serde::Deserialize;
use serde_json::{Map, Value};

const CLAUDE_CONFIG_JSON_ENV_VAR: &str = "LABRADOR_CLAUDE_CONFIG_JSON";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LocalClaudeConfigJson {
    oauth_account: Option<Map<String, Value>>,
}

pub fn has_auth_state() -> bool {
    has_auth_state_from_path(default_config_json_path()).unwrap_or(false)
}

fn default_config_json_path() -> PathBuf {
    env::var_os(CLAUDE_CONFIG_JSON_ENV_VAR)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".claude.json")))
        .unwrap_or_else(|| PathBuf::from(".claude.json"))
}

fn has_auth_state_from_path(path: PathBuf) -> Result<bool> {
    let json = fs::read_to_string(&path).with_context(|| {
        format!(
            "failed to read local Claude Code config JSON at {}",
            path.display()
        )
    })?;
    has_auth_state_from_json(&json)
}

fn has_auth_state_from_json(json: &str) -> Result<bool> {
    let config: LocalClaudeConfigJson =
        serde_json::from_str(json).context("failed to parse local Claude Code config JSON")?;
    let oauth_account = config
        .oauth_account
        .ok_or_else(|| anyhow!("local Claude Code config JSON is missing oauthAccount"))?;
    Ok(!oauth_account.is_empty())
}

#[cfg(test)]
#[path = "local_claude_auth_tests.rs"]
mod tests;
