use std::{path::PathBuf, sync::Mutex};

use super::{
    default_config_json_path, has_auth_state_from_json, CLAUDE_CONFIG_JSON_ENV_VAR,
    LEGACY_CLAUDE_CONFIG_JSON_ENV_VAR,
};

static ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn detects_claude_code_oauth_account() {
    let has_auth = has_auth_state_from_json(
        r#"{
            "hasCompletedOnboarding": true,
            "oauthAccount": {
                "accountUuid": "account",
                "emailAddress": "user@example.com"
            }
        }"#,
    )
    .expect("valid Claude config should parse");

    assert!(has_auth);
}

#[test]
fn default_config_json_path_prefers_labrador_env_var() {
    let _guard = ENV_LOCK.lock().unwrap();
    std::env::set_var(CLAUDE_CONFIG_JSON_ENV_VAR, "/tmp/labrador-claude.json");
    std::env::set_var(LEGACY_CLAUDE_CONFIG_JSON_ENV_VAR, "/tmp/legacy-claude.json");

    assert_eq!(
        default_config_json_path(),
        PathBuf::from("/tmp/labrador-claude.json")
    );

    std::env::remove_var(CLAUDE_CONFIG_JSON_ENV_VAR);
    std::env::remove_var(LEGACY_CLAUDE_CONFIG_JSON_ENV_VAR);
}

#[test]
fn default_config_json_path_falls_back_to_legacy_env_var() {
    let _guard = ENV_LOCK.lock().unwrap();
    std::env::remove_var(CLAUDE_CONFIG_JSON_ENV_VAR);
    std::env::set_var(
        LEGACY_CLAUDE_CONFIG_JSON_ENV_VAR,
        "/tmp/labrador-claude.json",
    );

    assert_eq!(
        default_config_json_path(),
        PathBuf::from("/tmp/labrador-claude.json")
    );

    std::env::remove_var(LEGACY_CLAUDE_CONFIG_JSON_ENV_VAR);
}

#[test]
fn rejects_missing_oauth_account() {
    let error = has_auth_state_from_json(
        r#"{
            "hasCompletedOnboarding": true
        }"#,
    )
    .expect_err("missing oauthAccount should be rejected");

    assert!(
        error.to_string().contains("oauthAccount"),
        "unexpected error: {error:#}"
    );
}

#[test]
fn rejects_empty_oauth_account() {
    let has_auth = has_auth_state_from_json(
        r#"{
            "oauthAccount": {}
        }"#,
    )
    .expect("empty oauthAccount should still parse");

    assert!(!has_auth);
}
