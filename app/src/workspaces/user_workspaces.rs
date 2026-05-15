use super::{
    team::Team,
    workspace::{
        AdminEnablementSetting, CustomerType, EnterpriseSecretRegex,
        UgcCollectionEnablementSetting, Workspace, WorkspaceUid,
    },
};
#[cfg(test)]
use super::team::MembershipRole;
use crate::{
    auth::UserUid,
    channel::ChannelState,
    report_error,
    server::{
        ids::ServerId,
        server_api::workspace::WorkspaceClient,
    },
    settings::{
        AISettings, AISettingsChangedEvent, CodeSettings, CodeSettingsChangedEvent, PrivacySettings,
    },
    workspaces::workspace::{
        AiAutonomySettings, AiOverages, SandboxedAgentSettings, UsageBasedPricingSettings,
    },
};
use anyhow::Result;
use regex::Regex;
use std::sync::Arc;
use warp_core::{
    features::FeatureFlag,
    settings::{ChangeEventReason, Setting},
};
use warp_graphql::workspace::FeatureModelChoice;
use warpui::{AppContext, Entity, ModelContext, SingletonEntity, Tracked};

#[cfg(test)]
use crate::server::server_api::workspace::MockWorkspaceClient;

#[cfg(test)]
use crate::workspaces::workspace::{
    AIAutonomyPolicy, BillingMetadata, WorkspaceMember, WorkspaceSettings,
};

#[cfg(test)]
use super::workspace::WorkspaceMemberUsageInfo;

const STRIPE_SUBSCRIPTION_INTERVAL_PAGE_PREFIX: &str = "/upgrade";

#[derive(Debug)]
pub enum UserWorkspacesEvent {
    GenerateStripeBillingPortalLink(String),
    GenerateStripeBillingPortalLinkRejected(anyhow::Error),
    UpdateWorkspaceSettingsSuccess,
    UpdateWorkspaceSettingsRejected(anyhow::Error),
    AiOveragesUpdated,
    /// Fired whenever the set of teams the user is on changes.
    TeamsChanged,
    CodebaseContextEnablementChanged,
    /// Fired when a service agreement's sunsetted_to_build_ts field is updated.
    SunsettedToBuildDataUpdated,
}

/// UserWorkspaces is a singleton model that holds workspace metadata (name, members, etc).
/// It should be used for getting information about the workspaces, teams, current teams,
/// and all other things related to operating on workspace and team data.
/// TODO: move other server_api calls to update_manager to correctly update sqlite.
pub struct UserWorkspaces {
    current_workspace_uid: Tracked<Option<WorkspaceUid>>,
    workspaces: Tracked<Vec<Workspace>>,
    workspace_client: Arc<dyn WorkspaceClient>,
}

/// Represents the workspaces a user potentially has access to.
#[derive(Clone)]
pub struct WorkspacesMetadataResponse {
    /// The list of workspaces the user is currently on.
    pub workspaces: Vec<Workspace>,
    /// TODO(Tyler): Post-workspaces, move this into the workspace object.
    /// Feature model choices may change from user to user and while the app is open, so we need to periodically update this list.
    /// It makes most sense to fetch this in workspaces which is queried every 10 minutes.
    /// This is list of available LLM models for the user.
    pub feature_model_choices: Option<FeatureModelChoice>,
}

// A representation of all data we fetch at a single time via our 10 minute poll.
// Prefer adding to this struct if you need relatively fresh data vs making
// independent queries.
pub struct WorkspacesMetadataWithPricing {
    pub metadata: WorkspacesMetadataResponse,
    pub pricing_info: Option<warp_graphql::billing::PricingInfo>,
}

impl UserWorkspaces {
    #[cfg(test)]
    pub fn mock(
        workspace_client: Arc<dyn WorkspaceClient>,
        cached_workspaces: Vec<Workspace>,
        _ctx: &mut ModelContext<Self>,
    ) -> Self {
        Self {
            current_workspace_uid: cached_workspaces.first().map(|w| w.uid).into(),
            workspaces: cached_workspaces.into(),
            workspace_client,
        }
    }

    #[cfg(test)]
    pub fn default_mock(ctx: &mut ModelContext<Self>) -> Self {
        Self::mock(Arc::new(MockWorkspaceClient::new()), vec![], ctx)
    }

    pub fn new(
        workspace_client: Arc<dyn WorkspaceClient>,
        cached_workspaces: Vec<Workspace>,
        current_workspace_uid: Option<WorkspaceUid>,
        ctx: &mut ModelContext<Self>,
    ) -> Self {
        ctx.subscribe_to_model(&CodeSettings::handle(ctx), |_, code_settings_event, ctx| {
            match code_settings_event {
                CodeSettingsChangedEvent::CodebaseContextEnabled { .. }
                | CodeSettingsChangedEvent::AutoIndexingEnabled { .. } => {
                    ctx.emit(UserWorkspacesEvent::CodebaseContextEnablementChanged);
                }
                _ => {}
            }
        });

        ctx.subscribe_to_model(&AISettings::handle(ctx), |_, ai_settings_event, ctx| {
            if let AISettingsChangedEvent::IsAnyAIEnabled { .. } = ai_settings_event {
                ctx.emit(UserWorkspacesEvent::CodebaseContextEnablementChanged);
            }
        });

        Self {
            current_workspace_uid: current_workspace_uid.into(),
            workspaces: cached_workspaces.into(),
            workspace_client,
        }
    }

    pub fn upgrade_link(user_id: UserUid) -> String {
        format!(
            "{}{}/{}/{}",
            ChannelState::server_root_url(),
            STRIPE_SUBSCRIPTION_INTERVAL_PAGE_PREFIX,
            "user",
            user_id.as_str()
        )
    }

    pub fn upgrade_link_for_team(team_uid: ServerId) -> String {
        format!(
            "{}{}/{}",
            ChannelState::server_root_url(),
            STRIPE_SUBSCRIPTION_INTERVAL_PAGE_PREFIX,
            team_uid
        )
    }

    pub fn workspace_from_uid(&self, workspace_uid: WorkspaceUid) -> Option<&Workspace> {
        self.workspaces.iter().find(|w| w.uid == workspace_uid)
    }

    pub fn workspace_from_uid_mut(
        &mut self,
        workspace_uid: WorkspaceUid,
    ) -> Option<&mut Workspace> {
        self.workspaces.iter_mut().find(|w| w.uid == workspace_uid)
    }

    /// Return the uid of user's current team (if any) without refreshing.
    pub fn current_team_uid(&self) -> Option<ServerId> {
        self.current_team().map(|t| t.uid)
    }

    pub fn current_team_mut(&mut self) -> Option<&mut Team> {
        self.current_workspace_mut()
            .and_then(|w| w.teams.first_mut())
    }

    /// Note that the team is populated with dummy data until
    /// the initial fetch completes (only team name and ID are cached in sqlite locally).
    /// Consider whether you need to wait for the results of the fetch before checking the
    /// values of other fields.
    pub fn current_team(&self) -> Option<&Team> {
        self.current_workspace().and_then(|w| w.teams.first())
    }

    /// Note that the workspace is populated with dummy data until the initial fetch
    /// completes (only workspace name/ID and workspace team's name/ID are cached in
    /// sqlite locally).
    /// Consider whether you need to wait for the results of the fetch before checking the
    /// values of other fields.
    pub fn current_workspace(&self) -> Option<&Workspace> {
        self.current_workspace_uid
            .and_then(|workspace_uid| self.workspace_from_uid(workspace_uid))
    }

    pub fn current_workspace_mut(&mut self) -> Option<&mut Workspace> {
        self.current_workspace_uid
            .and_then(|workspace_uid| self.workspace_from_uid_mut(workspace_uid))
    }

    pub fn workspaces(&self) -> &Vec<Workspace> {
        &self.workspaces
    }

    pub fn set_current_workspace_uid(
        &mut self,
        workspace_uid: WorkspaceUid,
        ctx: &mut ModelContext<Self>,
    ) {
        *self.current_workspace_uid = Some(workspace_uid);
        self.notify_and_emit_teams_changed(ctx);
    }

    /// Returns `true` if the current team's enterprise status allows AI features that have an
    /// enterprise gate. Non-enterprise teams always pass; enterprise teams pass only if they
    /// are on the Warp Plan or the build is dogfood (both our internal Warp team and dogfood
    /// team are billed as enterprise).
    pub fn ai_allowed_for_current_team(&self) -> bool {
        !self
            .current_team()
            .is_some_and(|team| team.billing_metadata.customer_type == CustomerType::Enterprise)
            || self
                .current_team()
                .is_some_and(|team| team.billing_metadata.is_warp_plan())
            || ChannelState::channel().is_dogfood()
    }

    /// Whether Prompt Suggestions should be toggleable for the current user, based on the active policies.
    /// Note that the value may be incorrect if called before the team's billing metadata has been fetched.
    pub fn is_prompt_suggestions_toggleable(&self) -> bool {
        self.current_team()
            // If the user has no team, they can toggle prompt suggestions (no restrictions).
            .is_none_or(|team| {
                team.billing_metadata
                    .tier
                    .warp_ai_policy
                    .is_some_and(|policy| policy.is_prompt_suggestions_toggleable)
            })
    }

    /// Whether Code Suggestions should be toggleable for the current user, based on the active policies.
    /// Note that the value may be incorrect if called before the team's billing metadata has been fetched.
    pub fn is_code_suggestions_toggleable(&self) -> bool {
        self.current_team()
            // If the user has no team, they can toggle code suggestions (no restrictions).
            .is_none_or(|team| {
                team.billing_metadata
                    .tier
                    .warp_ai_policy
                    .is_some_and(|policy| policy.is_code_suggestions_toggleable)
            })
    }

    /// Whether Next Command should be toggleable for the current user, based on the active policies.
    /// Note that the value may be incorrect if called before the team's billing metadata has been fetched.
    pub fn is_next_command_enabled(&self) -> bool {
        self.current_team()
            // If the user has no team, they can toggle Next Command (no restrictions).
            .is_none_or(|team| {
                team.billing_metadata
                    .tier
                    .warp_ai_policy
                    .is_some_and(|policy| policy.is_next_command_enabled)
            })
    }

    /// Whether voice input should be toggleable for the current user, based on the active policies.
    /// Note that the value may be incorrect if called before the team's billing metadata has been fetched.
    /// If voice input support is not compiled into this build, always returns `false`.
    pub fn is_voice_enabled(&self) -> bool {
        false
    }

    /// Whether BYO API key is enabled for the current user, based on the active policies.
    /// Note that the value may be incorrect if called before the team's billing metadata has been fetched.
    /// For solo users (no workspace), this is controlled by the `SoloUserByok` feature flag.
    pub fn is_byo_api_key_enabled(&self) -> bool {
        self.current_workspace()
            .map(|workspace| workspace.is_byo_api_key_enabled())
            .unwrap_or(FeatureFlag::SoloUserByok.is_enabled())
    }

    /// Returns the AI autonomy settings that are enforced by the workspace for all its members.
    /// If a setting is `None`, the workspace doesn't enforce a particular setting.
    pub fn ai_autonomy_settings(&self) -> AiAutonomySettings {
        self.current_team()
            .map(|team| team.organization_settings.ai_autonomy_settings.clone())
            .unwrap_or_default()
    }

    /// Returns the sandboxed agent settings enforced by the workspace, if any.
    pub fn sandboxed_agent_settings(&self) -> Option<SandboxedAgentSettings> {
        self.current_team()
            .and_then(|team| team.organization_settings.sandboxed_agent_settings.clone())
    }

    /// Returns true iff AI autonomy features are allowed for this client.
    /// TODO: This should be deleted soon. AI autonomy settings have been moved into organization
    /// settings (see `ai_autonomy_settings` above), but there could be an interim time where we
    /// have not set up the org settings yet for an enterprise that previously had the entire
    /// feature set disabled. To capture that case, we'll see if all the settings are `None`;
    /// if so, we'll fall back to their billing metadata's value. Once we've migrated everyone
    /// into org settings, we should remove `is_enabled` from the policy and delete this function.
    pub fn is_ai_autonomy_allowed(&self) -> bool {
        self.current_team().is_none_or(|team| {
            let settings = &team.organization_settings.ai_autonomy_settings;
            let all_settings_none = settings.apply_code_diffs_setting.is_none()
                && settings.read_files_setting.is_none()
                && settings.read_files_allowlist.is_none()
                && settings.execute_commands_setting.is_none()
                && settings.execute_commands_allowlist.is_none()
                && settings.execute_commands_denylist.is_none();

            if all_settings_none {
                team.billing_metadata
                    .tier
                    .ai_autonomy_policy
                    .is_some_and(|policy| policy.is_enabled)
            } else {
                true
            }
        })
    }

    pub fn update_workspaces(&mut self, workspaces: Vec<Workspace>, ctx: &mut ModelContext<Self>) {
        // Check if sunsetted_to_build_ts changed for any workspace
        let sunsetted_to_build_changed = self.has_sunsetted_to_build_data_changed(&workspaces);

        *self.workspaces = workspaces;
        self.notify_and_emit_teams_changed(ctx);

        if sunsetted_to_build_changed {
            ctx.emit(UserWorkspacesEvent::SunsettedToBuildDataUpdated);
        }
    }

    /// Checks if any workspace's service agreement sunsetted_to_build_ts field has changed.
    fn has_sunsetted_to_build_data_changed(&self, new_workspaces: &[Workspace]) -> bool {
        for new_workspace in new_workspaces {
            // Find the corresponding old workspace
            let old_workspace = self.workspaces.iter().find(|w| w.uid == new_workspace.uid);

            if let Some(old_workspace) = old_workspace {
                // Check if any team's service agreement sunsetted_to_build_ts changed
                for new_team in &new_workspace.teams {
                    let old_team = old_workspace.teams.iter().find(|t| t.uid == new_team.uid);

                    if let Some(old_team) = old_team {
                        let old_sunsetted = old_team
                            .billing_metadata
                            .service_agreements
                            .first()
                            .and_then(|sa| sa.sunsetted_to_build_ts);

                        let new_sunsetted = new_team
                            .billing_metadata
                            .service_agreements
                            .first()
                            .and_then(|sa| sa.sunsetted_to_build_ts);

                        // Detect if it changed from None to Some or changed value
                        if old_sunsetted != new_sunsetted {
                            return true;
                        }
                    }
                }
            }
        }
        false
    }

    fn notify_and_emit_teams_changed(&self, ctx: &mut ModelContext<Self>) {
        // Update session-sharing enablement since it depends on what teams the user
        // is part of.
        self.update_session_sharing_enablement(ctx);

        // PrivacySettings can't observe UserWorkspaces for updates, as it's initialized too early in
        // the app initialization flow. So, we update it manually whenever teams data changes.
        PrivacySettings::handle(ctx).update(ctx, |settings, ctx| {
            settings.set_is_telemetry_force_enabled(self.is_telemetry_force_enabled());
            settings.set_enterprise_secret_redaction_settings(
                self.is_enterprise_secret_redaction_enabled(),
                self.get_enterprise_secret_redaction_regex_list(),
                ChangeEventReason::CloudSync,
                ctx,
            );
        });

        ctx.emit(UserWorkspacesEvent::TeamsChanged);
        ctx.emit(UserWorkspacesEvent::CodebaseContextEnablementChanged);
        ctx.notify();
    }

    // TODO follow up with moving other modifying calls out of UserWorkspaces to TeamUpdateManager
    fn on_workspaces_updated(
        &mut self,
        result: Result<WorkspacesMetadataWithPricing>,
        ctx: &mut ModelContext<Self>,
    ) {
        match result {
            Ok(response) => {
                let workspaces = response.metadata.workspaces;

                self.update_workspaces(workspaces.clone(), ctx);

                // Check if the current workspace is still in the list of workspaces.
                // If it's not, then set the current workspace to the first workspace in the list.
                if let Some(current_workspace) = self.current_workspace() {
                    if !self
                        .workspaces
                        .iter()
                        .any(|w| w.uid == current_workspace.uid)
                    {
                        if let Some(workspace_uid) = workspaces.first().map(|w| w.uid) {
                            self.set_current_workspace_uid(workspace_uid, ctx);
                        }
                    }
                } else if let Some(workspace_uid) = workspaces.first().map(|w| w.uid) {
                    self.set_current_workspace_uid(workspace_uid, ctx);
                }
            }
            Err(e) => {
                report_error!(e.context("Failed to load user workspaces"));
            }
        }
    }

    pub fn on_generate_stripe_billing_portal_link(
        &mut self,
        result: Result<String>,
        ctx: &mut ModelContext<Self>,
    ) {
        match result {
            Err(err) => ctx.emit(UserWorkspacesEvent::GenerateStripeBillingPortalLinkRejected(err)),
            Ok(billing_session_link) => {
                ctx.emit(UserWorkspacesEvent::GenerateStripeBillingPortalLink(
                    billing_session_link,
                ));
            }
        };
        ctx.notify();
    }

    pub fn generate_stripe_billing_portal_link(
        &mut self,
        team_uid: ServerId,
        ctx: &mut ModelContext<Self>,
    ) {
        let workspace_client = self.workspace_client.clone();
        let _ = ctx.spawn(
            async move {
                workspace_client
                    .generate_stripe_billing_portal_link(team_uid)
                    .await
            },
            Self::on_generate_stripe_billing_portal_link,
        );
    }

    fn on_update_workspace_metadata(
        &mut self,
        result: Result<WorkspacesMetadataResponse>,
        ctx: &mut ModelContext<Self>,
    ) {
        match result {
            Ok(result) => {
                let wrapped = WorkspacesMetadataWithPricing {
                    metadata: result,
                    pricing_info: None,
                };
                self.on_workspaces_updated(Ok(wrapped), ctx);
                ctx.emit(UserWorkspacesEvent::UpdateWorkspaceSettingsSuccess);
            }
            Err(err) => {
                let err_for_event = anyhow::anyhow!("{}", err);
                self.on_workspaces_updated(Err(err), ctx);
                ctx.emit(UserWorkspacesEvent::UpdateWorkspaceSettingsRejected(
                    err_for_event,
                ));
            }
        };
        ctx.notify();
    }

    pub fn refresh_ai_overages(&mut self, ctx: &mut ModelContext<Self>) {
        let workspace_client = self.workspace_client.clone();
        let _ = ctx.spawn(
            async move { workspace_client.refresh_ai_overages().await },
            Self::on_refresh_ai_overages,
        );
    }

    pub fn update_addon_credits_settings(
        &mut self,
        team_uid: ServerId,
        auto_reload_enabled: Option<bool>,
        max_monthly_spend_cents: Option<i32>,
        selected_auto_reload_credit_denomination: Option<i32>,
        ctx: &mut ModelContext<Self>,
    ) {
        let workspace_client = self.workspace_client.clone();
        let _ = ctx.spawn(
            async move {
                workspace_client
                    .update_addon_credits_settings(
                        team_uid,
                        auto_reload_enabled,
                        max_monthly_spend_cents,
                        selected_auto_reload_credit_denomination,
                    )
                    .await
            },
            Self::on_update_workspace_metadata,
        );
    }

    fn on_refresh_ai_overages(&mut self, result: Result<AiOverages>, ctx: &mut ModelContext<Self>) {
        match result {
            Ok(fresh_ai_overages) => {
                // TODO: We really need to stop having duplicate billing metadata...
                if let Some(workspace) = self.current_workspace_mut() {
                    workspace.billing_metadata.ai_overages = Some(fresh_ai_overages.clone());
                }
                if let Some(team) = self.current_team_mut() {
                    team.billing_metadata.ai_overages = Some(fresh_ai_overages);
                }

                ctx.emit(UserWorkspacesEvent::AiOveragesUpdated);
                ctx.notify();
            }
            Err(e) => {
                log::warn!("Failed to refresh AI overages for workspace: {e:?}");
            }
        }
    }

    pub fn usage_based_pricing_settings(&self) -> UsageBasedPricingSettings {
        self.current_workspace()
            .map(|workspace| workspace.settings.usage_based_pricing_settings.clone())
            .unwrap_or_default()
    }

    pub fn is_telemetry_force_enabled(&self) -> bool {
        self.current_team()
            .map(|team| team.organization_settings.telemetry_settings.force_enabled)
            .unwrap_or(false)
    }

    pub fn is_enterprise_secret_redaction_enabled(&self) -> bool {
        self.current_team()
            .map(|team| team.organization_settings.secret_redaction_settings.enabled)
            .unwrap_or(false)
    }

    pub fn get_enterprise_secret_redaction_regex_list(&self) -> Vec<EnterpriseSecretRegex> {
        self.current_team()
            .map(|team| {
                team.organization_settings
                    .secret_redaction_settings
                    .regexes
                    .clone()
            })
            .unwrap_or_default()
    }

    pub fn get_ugc_collection_enablement_setting(&self) -> UgcCollectionEnablementSetting {
        self.current_team()
            .map(|team| {
                team.organization_settings
                    .ugc_collection_settings
                    .setting
                    .clone()
            })
            .unwrap_or_default()
    }

    pub fn get_cloud_conversation_storage_enablement_setting(&self) -> AdminEnablementSetting {
        self.current_team()
            .map(|team| {
                team.organization_settings
                    .cloud_conversation_storage_settings
                    .setting
                    .clone()
            })
            .unwrap_or_default()
    }

    pub fn is_ai_allowed_in_remote_sessions(&self) -> bool {
        self.current_team()
            .map(|team| {
                team.organization_settings
                    .ai_permissions_settings
                    .allow_ai_in_remote_sessions
            })
            .unwrap_or(true)
    }

    pub fn get_remote_session_regex_list(&self) -> Vec<Regex> {
        self.current_team()
            .map(|team| {
                team.organization_settings
                    .ai_permissions_settings
                    .remote_session_regex_list
                    .clone()
            })
            .unwrap_or_default()
    }

    pub fn is_anyone_with_link_sharing_enabled(&self) -> bool {
        self.current_team()
            .map(|team| {
                team.organization_settings
                    .link_sharing_settings
                    .anyone_with_link_sharing_enabled
            })
            .unwrap_or(true)
    }

    pub fn is_direct_link_sharing_enabled(&self) -> bool {
        self.current_team()
            .map(|team| {
                team.organization_settings
                    .link_sharing_settings
                    .direct_link_sharing_enabled
            })
            .unwrap_or(true)
    }

    /// Returns the codebase context settings, taking into account the organization,
    /// global AI settings, and codebase-specific settings.
    /// Prefer this function to determine whether to show indexing-related functionality.
    pub fn is_codebase_context_enabled(&self, app: &AppContext) -> bool {
        // If the organization has an explicit setting, respect it and make user toggle irrelevant.
        // - Enable: forced ON by org, regardless of user preference.
        // - Disable: forced OFF by org.
        // - RespectUserSetting: respect the user setting.
        let org_setting = self.team_allows_codebase_context();
        let ai_globally_enabled = AISettings::as_ref(app).is_any_ai_enabled(app);

        match org_setting {
            AdminEnablementSetting::Enable => ai_globally_enabled,
            AdminEnablementSetting::Disable => false,
            AdminEnablementSetting::RespectUserSetting => {
                ai_globally_enabled && *CodeSettings::as_ref(app).codebase_context_enabled.value()
            }
        }
    }

    /// Returns the team-level agent attribution setting.
    ///
    /// Use this to decide whether the user's attribution toggle should be locked
    /// (`Enable`/`Disable`) or editable (`RespectUserSetting`).
    pub fn get_agent_attribution_setting(&self) -> AdminEnablementSetting {
        self.current_team()
            .map(|team| team.organization_settings.enable_warp_attribution.clone())
            .unwrap_or_default()
    }

    /// Returns only the organization-specific codebase context enablement setting.
    /// Do not use this function to determine whether codebase context is generally enabled --
    /// use `is_codebase_context_enabled` instead.
    pub fn team_allows_codebase_context(&self) -> AdminEnablementSetting {
        self.current_team()
            .map(|team| {
                team.organization_settings
                    .codebase_context_settings
                    .setting
                    .clone()
            })
            .unwrap_or_default()
    }

    /// Updates whether or not session sharing is enabled based on the current team's tier policy.
    fn update_session_sharing_enablement(&self, ctx: &AppContext) {
        if cfg!(any(test, feature = "integration_tests")) {
            return;
        }

        let _ = (self, ctx);
        FeatureFlag::CreatingSharedSessions.set_enabled(false);
    }
}

#[cfg(test)]
impl UserWorkspaces {
    /// Creates a test workspace with a team and sets it as the current workspace.
    /// Returns the workspace UID and admin UID for use in tests.
    pub fn setup_test_workspace(&mut self, ctx: &mut ModelContext<Self>) {
        let workspace_uid = WorkspaceUid::from(ServerId::from(1));
        let owner_uid = UserUid::new("test_owner");

        let workspace_settings = WorkspaceSettings::default();

        let workspace = Workspace {
            uid: workspace_uid,
            name: "Test Workspace".to_string(),
            stripe_customer_id: None,
            teams: vec![Team {
                uid: ServerId::from(2),
                name: "Test Team".to_string(),
                organization_settings: workspace_settings.clone(),
                billing_metadata: BillingMetadata::default(),
                members: vec![],
                invite_code: None,
                pending_email_invites: vec![],
                invite_link_domain_restrictions: vec![],
                stripe_customer_id: None,
                is_eligible_for_discovery: false,
                has_billing_history: false,
            }],
            members: vec![WorkspaceMember {
                uid: owner_uid,
                email: "test@example.com".to_string(),
                role: MembershipRole::Owner,
                usage_info: WorkspaceMemberUsageInfo {
                    requests_used_since_last_refresh: 0,
                    request_limit: 1000,
                    is_unlimited: false,
                    is_request_limit_prorated: false,
                },
            }],
            billing_metadata: BillingMetadata::default(),
            bonus_grants_purchased_this_month: Default::default(),
            has_billing_history: false,
            settings: workspace_settings,
            invite_code: None,
            invite_link_domain_restrictions: vec![],
            pending_email_invites: vec![],
            is_eligible_for_discovery: false,
            total_requests_used_since_last_refresh: 0,
        };

        self.update_workspaces(vec![workspace], ctx);
        self.set_current_workspace_uid(workspace_uid, ctx);
    }

    /// Updates the current workspace by applying a mutation function.
    pub fn update_current_workspace<F>(&mut self, f: F, ctx: &mut ModelContext<Self>)
    where
        F: FnOnce(&mut Workspace),
    {
        if let Some(workspace) = self.current_workspace() {
            if workspace.teams.is_empty() {
                panic!("No team found in current workspace. Did you call setup_test_workspace()?");
            }

            let mut new_workspace = workspace.clone();
            f(&mut new_workspace);

            self.update_workspaces(vec![new_workspace], ctx);
        } else {
            panic!("No workspace found. Did you call setup_test_workspace()?");
        }
    }

    pub fn update_sandboxed_agent_settings<F>(&mut self, f: F, ctx: &mut ModelContext<Self>)
    where
        F: FnOnce(&mut Option<SandboxedAgentSettings>),
    {
        self.update_current_workspace(
            |workspace| {
                if let Some(team) = workspace.teams.first_mut() {
                    f(&mut team.organization_settings.sandboxed_agent_settings);
                } else {
                    panic!(
                        "No team found in current workspace. Did you call setup_test_workspace()?"
                    );
                }
            },
            ctx,
        );
    }

    pub fn update_ai_autonomy_settings<F>(&mut self, f: F, ctx: &mut ModelContext<Self>)
    where
        F: FnOnce(&mut AiAutonomySettings),
    {
        self.update_current_workspace(
            |workspace| {
                if let Some(team) = workspace.teams.first_mut() {
                    f(&mut team.organization_settings.ai_autonomy_settings);
                } else {
                    panic!(
                        "No team found in current workspace. Did you call setup_test_workspace()?"
                    );
                }
            },
            ctx,
        );
    }

    pub fn update_ai_autonomy_policy_flag(&mut self, enabled: bool, ctx: &mut ModelContext<Self>) {
        self.update_current_workspace(
            |workspace| {
                if let Some(team) = workspace.teams.first_mut() {
                    team.billing_metadata.tier.ai_autonomy_policy = Some(AIAutonomyPolicy {
                        is_enabled: enabled,
                        toggleable: true,
                    });
                } else {
                    panic!(
                        "No team found in current workspace. Did you call setup_test_workspace()?"
                    );
                }
            },
            ctx,
        );
    }
}

impl Entity for UserWorkspaces {
    type Event = UserWorkspacesEvent;
}

/// Mark UserWorkspaces as global application state.
impl SingletonEntity for UserWorkspaces {}

