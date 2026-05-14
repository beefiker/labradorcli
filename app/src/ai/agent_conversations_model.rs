use crate::ai::active_agent_views_model::ActiveAgentViewsModel;
use crate::ai::agent::conversation::{AIConversationId, ConversationStatus};
use crate::ai::artifacts::Artifact;
use crate::ai::blocklist::{format_credits, BlocklistAIHistoryEvent, BlocklistAIHistoryModel};
use crate::ai::conversation_navigation::ConversationNavigationData;
use crate::auth::AuthStateProvider;
use crate::ui_components::icons::Icon;
use crate::workspace::{RestoreConversationLayout, WorkspaceAction};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use session_sharing_protocol::common::SessionId;
use std::collections::HashMap;
use warp_cli::agent::Harness;
use warp_core::features::FeatureFlag;
use warp_core::ui::theme::{color::internal_colors, WarpTheme};
use warpui::color::ColorU;
use warpui::{
    AppContext, Entity, EntityId, ModelContext, SingletonEntity, WindowId,
};

#[derive(PartialEq)]
pub enum SessionStatus {
    Available,
    Expired,
    Unavailable,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum StatusFilter {
    #[default]
    All,
    Working,
    Done,
    Failed,
}

#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum SourceFilter {
    #[default]
    All,
}

#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum CreatorFilter {
    #[default]
    All,
    Specific {
        name: String,
        uid: String,
    },
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum ArtifactFilter {
    #[default]
    All,
    PullRequest,
    Plan,
    Screenshot,
    File,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum CreatedOnFilter {
    #[default]
    All,
    Last24Hours,
    Past3Days,
    LastWeek,
}

#[derive(Default, Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OwnerFilter {
    All,
    #[default]
    PersonalOnly,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub enum HarnessFilter {
    #[default]
    All,
    Specific(Harness),
}

impl Serialize for HarnessFilter {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            HarnessFilter::All => serializer.serialize_str("all"),
            HarnessFilter::Specific(harness) => serializer.collect_str(harness),
        }
    }
}

impl<'de> Deserialize<'de> for HarnessFilter {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Ok(<Harness as clap::ValueEnum>::from_str(&raw, false)
            .ok()
            .map(HarnessFilter::Specific)
            .unwrap_or(HarnessFilter::All))
    }
}

#[derive(Default, PartialEq, Eq, Clone, Debug, Serialize, Deserialize)]
pub struct AgentManagementFilters {
    pub owners: OwnerFilter,
    pub status: StatusFilter,
    pub source: SourceFilter,
    pub created_on: CreatedOnFilter,
    pub creator: CreatorFilter,
    pub artifact: ArtifactFilter,
    #[serde(default)]
    pub harness: HarnessFilter,
}

impl AgentManagementFilters {
    pub fn reset_all_but_owner(&mut self) {
        self.status = StatusFilter::default();
        self.source = SourceFilter::default();
        self.created_on = CreatedOnFilter::default();
        self.creator = CreatorFilter::default();
        self.artifact = ArtifactFilter::default();
        self.harness = HarnessFilter::default();
    }

    pub fn is_filtering(&self) -> bool {
        self.status != StatusFilter::default()
            || self.source != SourceFilter::default()
            || self.created_on != CreatedOnFilter::default()
            || self.creator != CreatorFilter::default() && self.owners != OwnerFilter::PersonalOnly
            || self.artifact != ArtifactFilter::default()
            || self.harness != HarnessFilter::default()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentRunDisplayStatus {
    ConversationInProgress,
    ConversationSucceeded,
    ConversationError,
    ConversationBlocked { blocked_action: String },
    ConversationCancelled,
}

impl AgentRunDisplayStatus {
    pub fn from_conversation_status(status: &ConversationStatus) -> Self {
        match status {
            ConversationStatus::InProgress => Self::ConversationInProgress,
            ConversationStatus::Success => Self::ConversationSucceeded,
            ConversationStatus::Error => Self::ConversationError,
            ConversationStatus::Cancelled => Self::ConversationCancelled,
            ConversationStatus::Blocked { blocked_action } => Self::ConversationBlocked {
                blocked_action: blocked_action.clone(),
            },
        }
    }

    pub fn status_filter(&self) -> StatusFilter {
        match self {
            AgentRunDisplayStatus::ConversationInProgress => StatusFilter::Working,
            AgentRunDisplayStatus::ConversationSucceeded => StatusFilter::Done,
            AgentRunDisplayStatus::ConversationError
            | AgentRunDisplayStatus::ConversationBlocked { .. }
            | AgentRunDisplayStatus::ConversationCancelled => StatusFilter::Failed,
        }
    }

    pub fn is_working(&self) -> bool {
        matches!(self, AgentRunDisplayStatus::ConversationInProgress)
    }

    pub fn status_icon_and_color(&self, theme: &WarpTheme) -> (Icon, ColorU) {
        match self {
            AgentRunDisplayStatus::ConversationInProgress => {
                (Icon::ClockLoader, theme.ansi_fg_magenta())
            }
            AgentRunDisplayStatus::ConversationSucceeded => (Icon::Check, theme.ansi_fg_green()),
            AgentRunDisplayStatus::ConversationError => (Icon::Triangle, theme.ansi_fg_red()),
            AgentRunDisplayStatus::ConversationBlocked { .. } => {
                (Icon::StopFilled, theme.ansi_fg_yellow())
            }
            AgentRunDisplayStatus::ConversationCancelled => {
                (Icon::StopFilled, internal_colors::neutral_5(theme))
            }
        }
    }
}

impl std::fmt::Display for AgentRunDisplayStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentRunDisplayStatus::ConversationInProgress => write!(f, "In progress"),
            AgentRunDisplayStatus::ConversationSucceeded => write!(f, "Done"),
            AgentRunDisplayStatus::ConversationError => write!(f, "Error"),
            AgentRunDisplayStatus::ConversationBlocked { .. } => write!(f, "Blocked"),
            AgentRunDisplayStatus::ConversationCancelled => write!(f, "Cancelled"),
        }
    }
}

/// Stores conversation metadata needed for display in conversation/task views.
pub struct ConversationMetadata {
    pub nav_data: ConversationNavigationData,
}

/// ConversationOrTask is a wrapper around conversation data stored in the
/// `AgentConversationsModel`.
pub enum ConversationOrTask<'a> {
    Conversation(&'a ConversationMetadata),
}

impl ConversationOrTask<'_> {
    pub fn title(&self, app: &AppContext) -> String {
        match self {
            ConversationOrTask::Conversation(metadata) => {
                let history_model = BlocklistAIHistoryModel::as_ref(app);
                history_model
                    .conversation(&metadata.nav_data.id)
                    .and_then(|conv| conv.title().clone())
                    .unwrap_or(metadata.nav_data.title.clone())
            }
        }
    }

    pub fn status(&self, app: &AppContext) -> ConversationStatus {
        match self {
            ConversationOrTask::Conversation(metadata) => {
                let history_model = BlocklistAIHistoryModel::as_ref(app);
                history_model
                    .conversation(&metadata.nav_data.id)
                    .map(|conv| conv.status().clone())
                    .unwrap_or(ConversationStatus::Success)
            }
        }
    }

    pub fn display_status(&self, app: &AppContext) -> AgentRunDisplayStatus {
        match self {
            ConversationOrTask::Conversation(metadata) => {
                let history_model = BlocklistAIHistoryModel::as_ref(app);
                history_model
                    .conversation(&metadata.nav_data.id)
                    .map(|conv| AgentRunDisplayStatus::from_conversation_status(conv.status()))
                    .unwrap_or(AgentRunDisplayStatus::ConversationSucceeded)
            }
        }
    }

    pub fn creator_name(&self, app: &AppContext) -> Option<String> {
        match self {
            ConversationOrTask::Conversation(_) => {
                AuthStateProvider::as_ref(app).get().username_for_display()
            }
        }
    }

    pub(super) fn request_usage(&self, app: &AppContext) -> Option<f32> {
        match self {
            ConversationOrTask::Conversation(metadata) => {
                let history_model = BlocklistAIHistoryModel::as_ref(app);
                history_model
                    .conversation(&metadata.nav_data.id)
                    .map(|conv| conv.credits_spent())
                    .or_else(|| {
                        history_model
                            .get_conversation_metadata(&metadata.nav_data.id)
                            .and_then(|m| m.credits_spent)
                    })
            }
        }
    }

    pub fn display_request_usage(&self, app: &AppContext) -> Option<String> {
        self.request_usage(app).map(format_credits)
    }

    pub fn last_updated(&self) -> DateTime<Utc> {
        match self {
            ConversationOrTask::Conversation(metadata) => metadata.nav_data.last_updated.into(),
        }
    }

    pub fn created_at(&self) -> DateTime<Utc> {
        match self {
            ConversationOrTask::Conversation(metadata) => metadata.nav_data.last_updated.into(),
        }
    }

    pub fn is_ambient_agent_conversation(&self) -> bool {
        false
    }

    pub fn navigation_data(&self) -> Option<&ConversationNavigationData> {
        match self {
            ConversationOrTask::Conversation(metadata) => Some(&metadata.nav_data),
        }
    }

    pub fn run_time(&self) -> Option<String> {
        None
    }

    pub fn harness(&self) -> Option<Harness> {
        None
    }

    pub fn artifacts(&self, app: &AppContext) -> Vec<Artifact> {
        match self {
            ConversationOrTask::Conversation(metadata) => {
                let history_model = BlocklistAIHistoryModel::as_ref(app);
                history_model
                    .conversation(&metadata.nav_data.id)
                    .map(|conv| conv.artifacts().to_vec())
                    .or_else(|| {
                        history_model
                            .get_conversation_metadata(&metadata.nav_data.id)
                            .map(|m| m.artifacts.clone())
                    })
                    .unwrap_or_default()
            }
        }
    }

    pub fn session_or_conversation_link(&self, _app: &AppContext) -> Option<String> {
        None
    }

    pub fn get_session_status(&self) -> Option<SessionStatus> {
        None
    }

    fn matches_status(&self, status_filter: &StatusFilter, app: &AppContext) -> bool {
        match status_filter {
            StatusFilter::All => true,
            StatusFilter::Working | StatusFilter::Done | StatusFilter::Failed => {
                self.display_status(app).status_filter() == *status_filter
            }
        }
    }

    fn matches_artifact(&self, artifact_filter: &ArtifactFilter, app: &AppContext) -> bool {
        artifacts_match_filter(&self.artifacts(app), artifact_filter)
    }

    fn matches_harness(&self, _harness_filter: &HarnessFilter) -> bool {
        true
    }

    fn matches_owner_and_creator(
        &self,
        owner_filter: &OwnerFilter,
        creator_filter: &CreatorFilter,
        app: &AppContext,
    ) -> bool {
        // Local conversations are always owned by the current user
        let passes_owner = match owner_filter {
            OwnerFilter::All | OwnerFilter::PersonalOnly => true,
        };

        if !passes_owner {
            return false;
        }

        if matches!(owner_filter, OwnerFilter::PersonalOnly) {
            return true;
        }

        match creator_filter {
            CreatorFilter::All => true,
            CreatorFilter::Specific { name, .. } => self.creator_name(app).as_ref() == Some(name),
        }
    }

    pub fn get_open_action(
        &self,
        restore_layout: Option<RestoreConversationLayout>,
        app: &AppContext,
    ) -> Option<WorkspaceAction> {
        match self {
            ConversationOrTask::Conversation(metadata) => {
                let is_active = ActiveAgentViewsModel::as_ref(app)
                    .is_conversation_open(metadata.nav_data.id, app);
                let nav_data = &metadata.nav_data;
                Some(WorkspaceAction::RestoreOrNavigateToConversation {
                    conversation_id: nav_data.id,
                    window_id: nav_data.window_id,
                    pane_view_locator: is_active
                        .then_some(nav_data.pane_view_locator)
                        .flatten(),
                    terminal_view_id: nav_data.terminal_view_id,
                    restore_layout,
                })
            }
        }
    }
}

pub(crate) fn artifacts_match_filter(
    artifacts: &[Artifact],
    artifact_filter: &ArtifactFilter,
) -> bool {
    match artifact_filter {
        ArtifactFilter::All => true,
        ArtifactFilter::PullRequest => artifacts
            .iter()
            .any(|artifact| matches!(artifact, Artifact::PullRequest { .. })),
        ArtifactFilter::Plan => artifacts
            .iter()
            .any(|artifact| matches!(artifact, Artifact::Plan { .. })),
        ArtifactFilter::Screenshot => artifacts
            .iter()
            .any(|artifact| matches!(artifact, Artifact::Screenshot { .. })),
        ArtifactFilter::File => artifacts
            .iter()
            .any(|artifact| matches!(artifact, Artifact::File { .. })),
    }
}

/// This model serves as a unified interface for reading local conversations.
pub struct AgentConversationsModel {
    /// A map of conversation IDs to local conversations.
    conversations: HashMap<AIConversationId, ConversationMetadata>,
    has_finished_initial_load: bool,
}

pub enum AgentConversationsModelEvent {
    /// Initial load of tasks completed.
    ConversationsLoaded,
    /// New tasks were received during polling (view should diff against its local state).
    NewTasksReceived,
    /// Existing task data may have been updated (e.g., state changes).
    TasksUpdated,
    /// Conversation status data was updated
    ConversationUpdated,
    /// Conversation artifacts were updated (plans, PRs, etc.)
    ConversationArtifactsUpdated { conversation_id: AIConversationId },
}

impl Entity for AgentConversationsModel {
    type Event = AgentConversationsModelEvent;
}

impl SingletonEntity for AgentConversationsModel {}

impl AgentConversationsModel {
    fn tracks_local_conversations() -> bool {
        FeatureFlag::InteractiveConversationManagementView.is_enabled()
            || FeatureFlag::AgentViewConversationListView.is_enabled()
    }

    pub fn new(ctx: &mut ModelContext<Self>) -> Self {
        let tracks_local_conversations = Self::tracks_local_conversations();

        if !tracks_local_conversations {
            return Self {
                conversations: HashMap::new(),
                has_finished_initial_load: true,
            };
        }

        let history_model = BlocklistAIHistoryModel::handle(ctx);
        ctx.subscribe_to_model(&history_model, move |me, event, ctx| {
            me.handle_history_event(event, ctx);
        });

        let active_views_model = ActiveAgentViewsModel::handle(ctx);
        ctx.subscribe_to_model(&active_views_model, |me, _event, ctx| {
            me.sync_conversations(ctx);
        });

        let mut model = Self {
            conversations: HashMap::new(),
            has_finished_initial_load: true,
        };

        model.sync_conversations(ctx);
        model
    }

    pub fn is_loading(&self) -> bool {
        !self.has_finished_initial_load
    }

    /// Sync all conversations to the AgentConversationsModel.
    pub fn sync_conversations(&mut self, ctx: &mut ModelContext<Self>) {
        if !Self::tracks_local_conversations() {
            return;
        }

        let nav_data_list = ConversationNavigationData::all_conversations(ctx);

        self.conversations.clear();
        for nav_data in nav_data_list {
            let conversation_id = nav_data.id;
            let metadata = ConversationMetadata { nav_data };
            self.conversations.insert(conversation_id, metadata);
        }

        ctx.emit(AgentConversationsModelEvent::ConversationsLoaded);
    }

    pub fn register_view_open(
        &mut self,
        _window_id: WindowId,
        _view_id: EntityId,
        _ctx: &mut ModelContext<Self>,
    ) {
    }

    pub fn register_view_closed(
        &mut self,
        _window_id: WindowId,
        _view_id: EntityId,
        _ctx: &mut ModelContext<Self>,
    ) {
    }

    fn handle_history_event(
        &mut self,
        event: &BlocklistAIHistoryEvent,
        ctx: &mut ModelContext<Self>,
    ) {
        if !Self::tracks_local_conversations() {
            return;
        }
        match event {
            BlocklistAIHistoryEvent::StartedNewConversation { .. }
            | BlocklistAIHistoryEvent::SetActiveConversation { .. }
            | BlocklistAIHistoryEvent::AppendedExchange { .. }
            | BlocklistAIHistoryEvent::SplitConversation { .. }
            | BlocklistAIHistoryEvent::RestoredConversations { .. }
            | BlocklistAIHistoryEvent::RemoveConversation { .. }
            | BlocklistAIHistoryEvent::DeletedConversation { .. }
            | BlocklistAIHistoryEvent::ClearedConversationsInTerminalView { .. }
            | BlocklistAIHistoryEvent::ClearedActiveConversation { .. } => {
                self.sync_conversations(ctx);
            }

            BlocklistAIHistoryEvent::UpdatedConversationStatus { .. } => {
                ctx.emit(AgentConversationsModelEvent::ConversationUpdated);
            }

            BlocklistAIHistoryEvent::UpdatedConversationArtifacts {
                conversation_id, ..
            } => {
                ctx.emit(AgentConversationsModelEvent::ConversationArtifactsUpdated {
                    conversation_id: *conversation_id,
                });
            }

            BlocklistAIHistoryEvent::CreatedSubtask { .. }
            | BlocklistAIHistoryEvent::UpgradedTask { .. }
            | BlocklistAIHistoryEvent::ReassignedExchange { .. }
            | BlocklistAIHistoryEvent::UpdatedTodoList { .. }
            | BlocklistAIHistoryEvent::UpdatedAutoexecuteOverride { .. }
            | BlocklistAIHistoryEvent::UpdatedConversationMetadata { .. }
            | BlocklistAIHistoryEvent::UpdatedStreamingExchange { .. }
            | BlocklistAIHistoryEvent::ConversationServerTokenAssigned { .. } => {}
        }
    }

    /// Returns true if we have local conversations.
    pub fn has_items(&self) -> bool {
        !self.conversations.is_empty()
    }

    /// Returns an iterator with all conversations with filters applied, sorted with the
    /// most recently updated items first.
    pub fn get_tasks_and_conversations(
        &self,
        filters: &AgentManagementFilters,
        app: &AppContext,
    ) -> impl Iterator<Item = ConversationOrTask<'_>> {
        let owner_creator_filter = move |t: &ConversationOrTask| {
            t.matches_owner_and_creator(&filters.owners, &filters.creator, app)
        };

        let status_filter = move |t: &ConversationOrTask| t.matches_status(&filters.status, app);

        let now = Utc::now();
        let created_cutoff = match filters.created_on {
            CreatedOnFilter::All => None,
            CreatedOnFilter::Last24Hours => Some(now - chrono::Duration::hours(24)),
            CreatedOnFilter::Past3Days => Some(now - chrono::Duration::days(3)),
            CreatedOnFilter::LastWeek => Some(now - chrono::Duration::days(7)),
        };

        let created_on_filter = move |t: &ConversationOrTask| match created_cutoff {
            Some(cutoff) => t.created_at() >= cutoff,
            None => true,
        };

        let artifact_filter_value = filters.artifact;
        let artifact_filter =
            move |t: &ConversationOrTask| t.matches_artifact(&artifact_filter_value, app);

        let harness_filter_value = filters.harness;
        let harness_filter = move |t: &ConversationOrTask| t.matches_harness(&harness_filter_value);

        let mut items: Vec<ConversationOrTask<'_>> = self
            .conversations
            .values()
            .map(ConversationOrTask::Conversation)
            .filter(owner_creator_filter)
            .filter(status_filter)
            .filter(created_on_filter)
            .filter(artifact_filter)
            .filter(harness_filter)
            .collect();

        items.sort_by(|a, b| b.last_updated().cmp(&a.last_updated()));
        items.into_iter()
    }

    /// Get a conversation by its AIConversationId
    pub fn get_conversation(
        &self,
        conversation_id: &AIConversationId,
    ) -> Option<ConversationOrTask<'_>> {
        self.conversations
            .get(conversation_id)
            .map(ConversationOrTask::Conversation)
    }

    /// Returns all (name, uid) pairs for creators.
    pub fn get_all_creators(&self, app: &AppContext) -> Vec<(String, String)> {
        let mut creators: Vec<(String, String)> = Vec::new();

        let auth_state = AuthStateProvider::as_ref(app).get();
        if let (Some(name), Some(uid)) = (auth_state.display_name(), auth_state.user_id()) {
            creators.push((name, uid.to_string()));
        }

        creators.sort_by(|a, b| a.0.cmp(&b.0));
        creators.dedup_by(|a, b| a.0 == b.0);

        creators
    }

    pub fn fetch_tasks_for_filters(
        &mut self,
        _filters: &AgentManagementFilters,
        _current_user_uid: &str,
        _ctx: &mut ModelContext<Self>,
    ) {
    }

    /// Clears all stored conversation data in memory.
    pub(crate) fn reset(&mut self) {
        self.conversations.clear();
        self.has_finished_initial_load = false;
    }
}

