use std::{env, fs, path::PathBuf};

use anyhow::{anyhow, Context as _, Result};
use serde::Deserialize;

const AUTH_JSON_ENV_VAR: &str = "DWARF_OPENAI_AUTH_JSON";

#[derive(Debug, Deserialize)]
struct LocalOpenAIAuthJson {
    auth_mode: Option<String>,
    tokens: Option<LocalOpenAITokens>,
}

#[derive(Debug, Deserialize)]
struct LocalOpenAITokens {
    access_token: Option<String>,
}

fn access_token() -> Option<String> {
    access_token_from_path(default_auth_json_path()?).ok()
}

pub fn has_access_token() -> bool {
    access_token().is_some()
}

fn default_auth_json_path() -> Option<PathBuf> {
    env::var_os(AUTH_JSON_ENV_VAR)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".codex").join("auth.json")))
}

fn access_token_from_path(path: PathBuf) -> Result<String> {
    let json = fs::read_to_string(&path).with_context(|| {
        format!(
            "failed to read local OpenAI auth JSON at {}",
            path.display()
        )
    })?;
    access_token_from_json(&json)
}

fn access_token_from_json(json: &str) -> Result<String> {
    let auth: LocalOpenAIAuthJson =
        serde_json::from_str(json).context("failed to parse local OpenAI auth JSON")?;

    match auth.auth_mode.as_deref() {
        Some("chatgpt") => {}
        Some(mode) => return Err(anyhow!("unsupported local OpenAI auth mode: {mode}")),
        None => return Err(anyhow!("local OpenAI auth JSON is missing auth_mode")),
    }

    auth.tokens
        .and_then(|tokens| tokens.access_token)
        .filter(|token| !token.trim().is_empty())
        .ok_or_else(|| anyhow!("local OpenAI auth JSON is missing tokens.access_token"))
}

#[cfg(test)]
#[path = "local_openai_auth_tests.rs"]
mod tests;
