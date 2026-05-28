use std::{path::PathBuf, sync::Mutex};

use super::{
    access_token_from_json, default_auth_json_path, AUTH_JSON_ENV_VAR, LEGACY_AUTH_JSON_ENV_VAR,
};

static ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn reads_access_token_from_codex_auth_json() {
    let token = access_token_from_json(
        r#"{
            "auth_mode": "chatgpt",
            "OPENAI_API_KEY": null,
            "tokens": {
                "id_token": "id-token",
                "access_token": "local-access-token",
                "refresh_token": "refresh-token",
                "account_id": "account"
            },
            "last_refresh": "2026-04-23T02:46:13.163Z"
        }"#,
    )
    .expect("valid auth JSON should return the access token");

    assert_eq!(token, "local-access-token");
}

#[test]
fn default_auth_json_path_prefers_labrador_env_var() {
    let _guard = ENV_LOCK.lock().unwrap();
    std::env::set_var(AUTH_JSON_ENV_VAR, "/tmp/labrador-openai-auth.json");
    std::env::set_var(LEGACY_AUTH_JSON_ENV_VAR, "/tmp/legacy-openai-auth.json");

    assert_eq!(
        default_auth_json_path(),
        Some(PathBuf::from("/tmp/labrador-openai-auth.json"))
    );

    std::env::remove_var(AUTH_JSON_ENV_VAR);
    std::env::remove_var(LEGACY_AUTH_JSON_ENV_VAR);
}

#[test]
fn default_auth_json_path_falls_back_to_legacy_env_var() {
    let _guard = ENV_LOCK.lock().unwrap();
    std::env::remove_var(AUTH_JSON_ENV_VAR);
    std::env::set_var(LEGACY_AUTH_JSON_ENV_VAR, "/tmp/labrador-openai-auth.json");

    assert_eq!(
        default_auth_json_path(),
        Some(PathBuf::from("/tmp/labrador-openai-auth.json"))
    );

    std::env::remove_var(LEGACY_AUTH_JSON_ENV_VAR);
}

#[test]
fn rejects_missing_access_token() {
    let error = access_token_from_json(
        r#"{
            "auth_mode": "chatgpt",
            "OPENAI_API_KEY": null,
            "tokens": {
                "id_token": "id-token",
                "refresh_token": "refresh-token"
            }
        }"#,
    )
    .expect_err("missing access token should be rejected");

    assert!(
        error.to_string().contains("tokens.access_token"),
        "unexpected error: {error:#}"
    );
}

#[test]
fn ignores_openai_api_key_field() {
    let error = access_token_from_json(
        r#"{
            "auth_mode": "chatgpt",
            "OPENAI_API_KEY": "sk-fake-key"
        }"#,
    )
    .expect_err("OPENAI_API_KEY should not be used as a fallback");

    assert!(
        error.to_string().contains("tokens.access_token"),
        "unexpected error: {error:#}"
    );
}

#[test]
fn rejects_unsupported_auth_mode() {
    let error = access_token_from_json(
        r#"{
            "auth_mode": "api_key",
            "tokens": {
                "access_token": "local-access-token"
            }
        }"#,
    )
    .expect_err("unsupported auth mode should be rejected");

    assert!(
        error
            .to_string()
            .contains("unsupported local OpenAI auth mode"),
        "unexpected error: {error:#}"
    );
}
