use warp_graphql::ai::{AgentTaskState, PlatformErrorCode};

use super::classify_driver_error;
use crate::ai::agent_sdk::driver::terminal::ShareSessionError;
use crate::ai::agent_sdk::driver::AgentDriverError;

fn assert_state_and_code(
    error: AgentDriverError,
    expected_state: AgentTaskState,
    expected_code: Option<PlatformErrorCode>,
) {
    let (state, update) = classify_driver_error(&error);
    assert_eq!(state, expected_state, "unexpected state for {error}");
    assert_eq!(
        update.error_code, expected_code,
        "unexpected error_code for {error}"
    );
}

// --- Infrastructure errors → ERROR ---

#[test]
fn bootstrap_failed_is_error_with_internal() {
    assert_state_and_code(
        AgentDriverError::BootstrapFailed,
        AgentTaskState::Error,
        Some(PlatformErrorCode::InternalError),
    );
}

#[test]
fn terminal_unavailable_is_error_with_internal() {
    assert_state_and_code(
        AgentDriverError::TerminalUnavailable,
        AgentTaskState::Error,
        Some(PlatformErrorCode::InternalError),
    );
}

#[test]
fn not_logged_in_is_error_with_auth_required() {
    let (state, update) = classify_driver_error(&AgentDriverError::NotLoggedIn);
    assert_eq!(state, AgentTaskState::Error);
    assert_eq!(
        update.error_code,
        Some(PlatformErrorCode::AuthenticationRequired)
    );
    assert!(
        update.message.contains("WARP_API_KEY"),
        "message should mention WARP_API_KEY: {:?}",
        update.message
    );
}

#[test]
fn warp_drive_sync_failed_is_error() {
    assert_state_and_code(
        AgentDriverError::WarpDriveSyncFailed,
        AgentTaskState::Error,
        Some(PlatformErrorCode::InternalError),
    );
}

// --- ShareSessionFailed variants ---

#[test]
fn share_session_disabled_gets_feature_not_available() {
    let (state, update) = classify_driver_error(&AgentDriverError::ShareSessionFailed {
        error: ShareSessionError::Disabled,
    });
    assert_eq!(state, AgentTaskState::Error);
    assert_eq!(
        update.error_code,
        Some(PlatformErrorCode::FeatureNotAvailable)
    );
    assert!(update.message.contains("not enabled"));
    assert!(update.message.contains("--share flag"));
}

#[test]
fn share_session_timeout_gets_internal_error() {
    let (state, update) = classify_driver_error(&AgentDriverError::ShareSessionFailed {
        error: ShareSessionError::Timeout,
    });
    assert_eq!(state, AgentTaskState::Error);
    assert_eq!(update.error_code, Some(PlatformErrorCode::InternalError));
    assert!(update.message.contains("timed out"));
}

#[test]
fn share_session_failed_includes_reason() {
    let (state, update) = classify_driver_error(&AgentDriverError::ShareSessionFailed {
        error: ShareSessionError::Failed("server rejected".into()),
    });
    assert_eq!(state, AgentTaskState::Error);
    assert!(update.message.contains("server rejected"));
}

