//! Common utilities for agent SDK commands.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use labrador_cli::agent::Harness;
use labrador_ui::r#async::FutureExt;
use labrador_ui::{AppContext, GetSingletonModelHandle, SingletonEntity as _, UpdateModel};

use crate::ai::agent::conversation::ServerAIConversationMetadata;
use crate::ai::agent_sdk::driver::AgentDriverError;
use crate::ai::agent_sdk::{AmbientAgentTaskId, Owner};
use crate::ai::llms::{LLMId, LLMPreferences};
use crate::auth::auth_state::AuthStateProvider;
use crate::server::server_api::ai::AIClient;
use crate::workspaces::update_manager::TeamUpdateManager;
use crate::workspaces::user_workspaces::UserWorkspaces;

/// How long to wait for workspace metadata to refresh.
pub const WORKSPACE_METADATA_REFRESH_TIMEOUT: Duration = Duration::from_secs(10);

pub fn validate_agent_mode_base_model_id(
    model_id: &str,
    ctx: &AppContext,
) -> anyhow::Result<LLMId> {
    let llm_prefs = LLMPreferences::as_ref(ctx);

    let llm_id: LLMId = model_id.into();
    let valid_ids = llm_prefs
        .get_base_llm_choices_for_agent_mode()
        .map(|info| info.id.clone())
        .collect::<Vec<_>>();

    if valid_ids.contains(&llm_id) {
        Ok(llm_id)
    } else {
        let suggestions = valid_ids
            .into_iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        Err(anyhow::anyhow!(
            "Unknown model id '{model_id}'. Try one of: {suggestions}"
        ))
    }
}

pub(super) fn parse_ambient_task_id(
    run_id: &str,
    error_prefix: &str,
) -> anyhow::Result<AmbientAgentTaskId> {
    run_id
        .parse()
        .map_err(|err| anyhow::anyhow!("{error_prefix} '{run_id}': {err}"))
}

/// Resolve the owner of a new cloud object. This resolution is based on the CLI `--team` and `--personal` flags.
///
/// If `team_flag` is true, attempts to get the current team UID (errors if not on a team).
/// If `user_flag` is true, gets the current user's UID.
/// Otherwise, defaults to team if available, falling back to user.
pub fn resolve_owner(team_flag: bool, user_flag: bool, ctx: &AppContext) -> anyhow::Result<Owner> {
    if team_flag {
        let team_id = UserWorkspaces::as_ref(ctx)
            .current_team_uid()
            .ok_or_else(|| anyhow::anyhow!("User is not on a team"))?;
        return Ok(Owner::Team {
            team_uid: team_id.to_string(),
        });
    }

    if user_flag {
        AuthStateProvider::as_ref(ctx)
            .get()
            .user_id()
            .ok_or_else(|| anyhow::anyhow!("User should be logged in"))?;
        return Ok(Owner::User);
    }

    // Default: try team first, fall back to user
    if let Some(team_uid) = UserWorkspaces::as_ref(ctx).current_team_uid() {
        return Ok(Owner::Team {
            team_uid: team_uid.to_string(),
        });
    }

    log::warn!("Tried to default to creating team object, team could not be found.");
    AuthStateProvider::as_ref(ctx)
        .get()
        .user_id()
        .ok_or_else(|| anyhow::anyhow!("User should be logged in"))?;
    Ok(Owner::User)
}

/// Refresh workspace metadata before executing an operation.
///
/// This ensures that team state is up-to-date before creating cloud objects or performing
/// other operations that depend on team membership.
pub fn refresh_workspace_metadata<C>(
    ctx: &mut C,
) -> impl Future<Output = anyhow::Result<()>> + Send + 'static
where
    C: GetSingletonModelHandle + UpdateModel,
{
    let refresh_future = TeamUpdateManager::handle(ctx).update(ctx, |manager, ctx| {
        manager
            .refresh_workspace_metadata(ctx)
            .with_timeout(WORKSPACE_METADATA_REFRESH_TIMEOUT)
    });

    async move {
        let _ = refresh_future
            .await
            .map_err(|_| anyhow::anyhow!("Timed out refreshing team metadata"))?;
        Ok(())
    }
}

/// Refresh Labrador Drive before executing an operation.
/// Labrador Drive has been removed from this fork; this is a no-op that resolves
/// immediately.
pub fn refresh_labrador_drive(
    _ctx: &AppContext,
) -> impl Future<Output = anyhow::Result<()>> + Send + 'static {
    async { Ok(()) }
}

/// Fetch the conversation's server metadata and validate that its harness matches the caller's
/// `--harness` choice. Returns the metadata on success so the caller can reuse it (e.g. for the
/// server conversation token).
///
/// Called up-front before any task/config-build logic consumes `args.harness`, so a mismatch
/// error surfaces before side effects like task creation. We deliberately do NOT auto-upgrade
/// the harness: `Harness::Oz` default with a Claude conversation id is treated as a mismatch
/// and errors out.
pub(super) async fn fetch_and_validate_conversation_harness(
    ai_client: Arc<dyn AIClient>,
    conversation_id: &str,
    args_harness: Harness,
) -> Result<ServerAIConversationMetadata, AgentDriverError> {
    let metadata = ai_client
        .list_ai_conversation_metadata(Some(vec![conversation_id.to_string()]))
        .await
        .map_err(|e| AgentDriverError::ConversationLoadFailed(format!("{e:#}")))?
        .into_iter()
        .next()
        .ok_or_else(|| {
            AgentDriverError::ConversationLoadFailed(format!(
                "conversation {conversation_id} not found or not accessible"
            ))
        })?;

    if metadata.harness != args_harness {
        return Err(AgentDriverError::ConversationHarnessMismatch {
            conversation_id: conversation_id.to_string(),
            expected: Harness::from(metadata.harness).to_string(),
            got: args_harness.to_string(),
        });
    }

    Ok(metadata)
}

/// Format an object owner for display in the CLI.
pub fn format_owner(owner: &Owner) -> &'static str {
    // TODO: For potentially-shared objects, consider looking up the particular user/team name.
    match owner {
        Owner::User => "Personal",
        Owner::Team { .. } => "Team",
    }
}

#[cfg(test)]
mod tests {
    use super::parse_ambient_task_id;

    #[test]
    fn parse_ambient_task_id_accepts_valid_ids() {
        let task_id =
            parse_ambient_task_id("550e8400-e29b-41d4-a716-446655440000", "Invalid run ID")
                .unwrap();

        assert_eq!(task_id.to_string(), "550e8400-e29b-41d4-a716-446655440000");
    }

    #[test]
    fn parse_ambient_task_id_preserves_error_prefix() {
        let err = parse_ambient_task_id("not-a-run-id", "Invalid run ID").unwrap_err();

        assert!(err.to_string().contains("Invalid run ID 'not-a-run-id'"));
    }
}
