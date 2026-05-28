use super::{team::TeamClient, ServerApi};
use crate::workspaces::user_workspaces::WorkspacesMetadataResponse;
use crate::workspaces::workspace::AiOverages;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use cynic::{MutationBuilder, QueryBuilder};
use labrador_graphql::mutations::update_workspace_settings::{
    AddonCreditsSettingsInput, UpdateWorkspaceSettings, UpdateWorkspaceSettingsInput,
    UpdateWorkspaceSettingsResult, UpdateWorkspaceSettingsVariables,
};
use labrador_graphql::queries::get_ai_overages_for_workspace::{
    GetAiOveragesForWorkspace, GetAiOveragesForWorkspaceVariables, UserResult,
};

use crate::server::graphql::{get_request_context, get_user_facing_error_message};
use crate::server::ids::ServerId;

#[cfg(test)]
use mockall::{automock, predicate::*};

#[cfg_attr(test, automock)]
#[cfg_attr(not(target_family = "wasm"), async_trait)]
#[cfg_attr(target_family = "wasm", async_trait(?Send))]
pub trait WorkspaceClient: 'static + Send + Sync {
    async fn refresh_ai_overages(&self) -> Result<AiOverages>;

    async fn update_addon_credits_settings(
        &self,
        team_uid: ServerId,
        auto_reload_enabled: Option<bool>,
        max_monthly_spend_cents: Option<i32>,
        selected_auto_reload_credit_denomination: Option<i32>,
    ) -> Result<WorkspacesMetadataResponse>;
}

#[cfg_attr(not(target_family = "wasm"), async_trait)]
#[cfg_attr(target_family = "wasm", async_trait(?Send))]
impl WorkspaceClient for ServerApi {
    async fn refresh_ai_overages(&self) -> Result<AiOverages> {
        let variables = GetAiOveragesForWorkspaceVariables {
            request_context: get_request_context(),
        };
        let operation = GetAiOveragesForWorkspace::build(variables);
        let response = self.send_graphql_request(operation, None).await?;

        match response.user {
            UserResult::UserOutput(user_output) => user_output
                .user
                .workspaces
                .first()
                .as_ref()
                .ok_or_else(|| anyhow!("No workspace found"))?
                .billing_metadata
                .ai_overages
                .as_ref()
                .ok_or_else(|| anyhow!("No AI overages found"))
                .map(|overages| AiOverages {
                    current_monthly_request_cost_cents: overages.current_monthly_request_cost_cents,
                    current_monthly_requests_used: overages.current_monthly_requests_used,
                    current_period_end: overages.current_period_end.utc(),
                }),
            UserResult::Unknown => Err(anyhow!("Unknown error")),
        }
    }

    async fn update_addon_credits_settings(
        &self,
        team_uid: ServerId,
        auto_reload_enabled: Option<bool>,
        max_monthly_spend_cents: Option<i32>,
        selected_auto_reload_credit_denomination: Option<i32>,
    ) -> Result<WorkspacesMetadataResponse> {
        let variables = UpdateWorkspaceSettingsVariables {
            input: UpdateWorkspaceSettingsInput {
                workspace_uid: team_uid.to_string(),
                set_usage_based_pricing_settings: None,
                set_addon_credits_settings: Some(AddonCreditsSettingsInput {
                    auto_reload_enabled,
                    max_monthly_spend_cents,
                    selected_auto_reload_credit_denomination,
                }),
            },
            request_context: get_request_context(),
        };
        let operation = UpdateWorkspaceSettings::build(variables);
        let response = self.send_graphql_request(operation, None).await?;

        match response.update_workspace_settings {
            UpdateWorkspaceSettingsResult::UpdateWorkspaceSettingsOutput(_) => {
                TeamClient::workspaces_metadata(self)
                    .await
                    .map(|w| w.metadata)
            }
            UpdateWorkspaceSettingsResult::UserFacingError(error) => {
                Err(anyhow!(get_user_facing_error_message(error)))
            }
            UpdateWorkspaceSettingsResult::Unknown => Err(anyhow!("Unknown error")),
        }
    }
}
