use std::{
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Context as _, Result};
use serde::Deserialize;

const AUTH_JSON_ENV_VAR: &str = "LABRADOR_OPENAI_AUTH_JSON";
const LEGACY_AUTH_JSON_ENV_VAR: &str = "DWARF_OPENAI_AUTH_JSON";
const CODEX_HOME_DIR_NAME: &str = "codex-home";
const CODEX_AUTH_JSON_FILE_NAME: &str = "auth.json";

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
        .or_else(|| {
            env::var_os(LEGACY_AUTH_JSON_ENV_VAR)
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
        })
        .or_else(|| dirs::home_dir().map(|home| home.join(".codex").join("auth.json")))
}

/// Builds a Labrador-owned Codex home containing only the local OAuth file.
///
/// We use this when launching Codex as an embedded local agent so Codex can
/// authenticate without loading the user's Codex config, MCP servers, or skills.
pub fn prepare_isolated_codex_home() -> Option<PathBuf> {
    let auth_json_path = default_auth_json_path()?;
    let isolated_home = labrador_core::paths::state_dir().join(CODEX_HOME_DIR_NAME);
    match copy_auth_json_to_isolated_codex_home(&auth_json_path, &isolated_home) {
        Ok(path) => Some(path),
        Err(error) => {
            log::warn!(
                "Unable to prepare isolated Codex home at {}: {error:#}",
                isolated_home.display()
            );
            None
        }
    }
}

fn copy_auth_json_to_isolated_codex_home(
    auth_json_path: &Path,
    isolated_home: &Path,
) -> Result<PathBuf> {
    if !auth_json_path.is_file() {
        anyhow::bail!(
            "Codex auth JSON does not exist at {}",
            auth_json_path.display()
        );
    }

    fs::create_dir_all(isolated_home).with_context(|| {
        format!(
            "failed to create isolated Codex home at {}",
            isolated_home.display()
        )
    })?;

    let isolated_auth_json_path = isolated_home.join(CODEX_AUTH_JSON_FILE_NAME);
    if auth_json_path == isolated_auth_json_path {
        return Ok(isolated_home.to_path_buf());
    }

    fs::copy(auth_json_path, &isolated_auth_json_path).with_context(|| {
        format!(
            "failed to copy Codex auth JSON from {} to {}",
            auth_json_path.display(),
            isolated_auth_json_path.display()
        )
    })?;

    Ok(isolated_home.to_path_buf())
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
