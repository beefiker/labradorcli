use super::ServerApi;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use cynic::QueryBuilder;
use labrador_graphql::queries::get_workspaces_metadata_for_user::{
    GetWorkspacesMetadataForUser, GetWorkspacesMetadataForUserVariables,
};

use crate::server::graphql::get_request_context;
use crate::workspaces::user_workspaces::WorkspacesMetadataWithPricing;

#[cfg(test)]
use mockall::{automock, predicate::*};

#[cfg_attr(test, automock)]
#[cfg_attr(not(target_family = "wasm"), async_trait)]
#[cfg_attr(target_family = "wasm", async_trait(?Send))]
pub trait TeamClient: 'static + Send + Sync {
    async fn workspaces_metadata(&self) -> Result<WorkspacesMetadataWithPricing>;
}

#[cfg_attr(not(target_family = "wasm"), async_trait)]
#[cfg_attr(target_family = "wasm", async_trait(?Send))]
impl TeamClient for ServerApi {
    async fn workspaces_metadata(&self) -> Result<WorkspacesMetadataWithPricing> {
        let variables = GetWorkspacesMetadataForUserVariables {
            request_context: get_request_context(),
        };
        let operation = GetWorkspacesMetadataForUser::build(variables);
        let response = self.send_graphql_request(operation, None).await?;

        let metadata = match response.user {
            labrador_graphql::queries::get_workspaces_metadata_for_user::UserResult::UserOutput(
                user_output,
            ) => user_output.user.into(),
            labrador_graphql::queries::get_workspaces_metadata_for_user::UserResult::Unknown => {
                return Err(anyhow!("Unable to fetch workspaces metadata"));
            }
        };

        Ok(WorkspacesMetadataWithPricing { metadata })
    }
}
