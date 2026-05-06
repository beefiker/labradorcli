use super::has_auth_state_from_json;

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
