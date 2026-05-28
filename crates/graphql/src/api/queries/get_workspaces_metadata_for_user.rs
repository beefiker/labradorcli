use crate::{
    billing::PricingInfo, experiment::Experiment, request_context::RequestContext, schema,
    user::DiscoverableTeamData, workspace::Workspace,
};

// The server still exposes legacy billing field names; keep Rust-facing
// fragments Labrador-named and map them at the field/type boundary.

#[derive(cynic::QueryVariables, Debug)]
pub struct GetWorkspacesMetadataForUserVariables {
    pub request_context: RequestContext,
}

#[derive(cynic::QueryFragment, Debug)]
pub struct UserOutput {
    pub user: User,
}

#[derive(cynic::InlineFragments, Debug)]
pub enum UserResult {
    UserOutput(UserOutput),
    #[cynic(fallback)]
    Unknown,
}

#[derive(cynic::QueryFragment, Debug)]
pub struct PricingInfoOutput {
    pub pricing_info: PricingInfo,
}

#[derive(cynic::InlineFragments, Debug)]
pub enum PricingInfoResult {
    PricingInfoOutput(PricingInfoOutput),
    #[cynic(fallback)]
    Unknown,
}

#[derive(cynic::QueryFragment, Debug)]
pub struct User {
    pub workspaces: Vec<Workspace>,
    pub experiments: Option<Vec<Experiment>>,
    pub discoverable_teams: Vec<DiscoverableTeamData>,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(
    graphql_type = "RootQuery",
    variables = "GetWorkspacesMetadataForUserVariables"
)]
pub struct GetWorkspacesMetadataForUser {
    #[arguments(requestContext: $request_context)]
    pub user: UserResult,
    #[arguments(requestContext: $request_context)]
    pub pricing_info: PricingInfoResult,
}
crate::client::define_operation! {
    get_workspaces_metadata_for_user(GetWorkspacesMetadataForUserVariables) -> GetWorkspacesMetadataForUser;
}
