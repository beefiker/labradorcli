pub(crate) mod convert_conversation;
mod convert_from;
mod convert_to;
mod r#impl;
mod local_codex;

pub use ai::agent::convert::ConvertToAPITypeError;
use ai::api_keys::ApiKeyManager;
pub use convert_from::{
    user_inputs_from_messages, ConversionParams, ConvertAPIMessageToClientOutputMessage,
    MaybeAIAgentOutputMessage, MessageToAIAgentOutputMessageError,
};
pub(crate) use local_codex::{
    maybe_generate_local_ai_input_suggestions, maybe_generate_local_am_query_suggestions,
};

pub use r#impl::generate_multi_agent_output;

use futures_lite::Stream;
use labrador_core::channel::ChannelState;
use labrador_core::execution_mode::AppExecutionMode;
use labrador_core::features::FeatureFlag;
use labrador_multi_agent_api::{
    message::tool_call::subagent::Metadata as SubagentMetadata,
    request::{settings::ApiKeys as RequestApiKeys, Settings as RequestSettings},
    response_event::stream_finished::{
        ConversationUsageMetadata as ApiConversationUsageMetadata,
        ModelTokenUsage as ApiModelTokenUsage, ToolUsageMetadata as ApiToolUsageMetadata,
    },
};
use serde::Serialize;
use std::collections::HashMap;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;

use crate::ai::agent::conversation::AIConversationId;
use crate::{
    ai::{blocklist::SessionContext, llms::LLMId},
    server::server_api::AIApiError,
};

use super::{AIAgentInput, MCPContext, MCPServer, RequestMetadata, Suggestions};
use crate::ai::blocklist::{BlocklistAIPermissions, RequestInput};
use crate::ai::execution_profiles::profiles::AIExecutionProfilesModel;
use crate::ai::mcp::templatable_manager::TemplatableMCPServerInfo;
use crate::ai::mcp::TemplatableMCPServerManager;
use crate::settings::AISettings;
use crate::terminal::safe_mode_settings::get_secret_obfuscation_mode;
use crate::workspaces::user_workspaces::UserWorkspaces;
use labrador_core::user_preferences::GetUserPreferences;
use labrador_ui::{AppContext, EntityId, SingletonEntity as _};

#[cfg(test)]
pub(crate) fn labrador_documentation_search_metadata() -> SubagentMetadata {
    SubagentMetadata::WarpDocumentationSearch(())
}

pub(crate) fn is_labrador_documentation_search_metadata(metadata: &SubagentMetadata) -> bool {
    matches!(metadata, SubagentMetadata::WarpDocumentationSearch(_))
}

pub(crate) fn set_labrador_credit_usage(api_keys: &mut RequestApiKeys, allow: bool) {
    api_keys.allow_use_of_warp_credits = allow;
}

pub(crate) fn set_labrador_drive_context_enabled(
    settings: &mut RequestSettings,
    enabled: bool,
) {
    settings.warp_drive_context_enabled = enabled;
}

pub(crate) fn conversation_usage_metadata(
    context_window_usage: f32,
    summarized: bool,
    credits_spent: f32,
    token_usage: Vec<ApiModelTokenUsage>,
    tool_usage_metadata: Option<ApiToolUsageMetadata>,
    labrador_token_usage: HashMap<String, ApiModelTokenUsage>,
    byok_token_usage: HashMap<String, ApiModelTokenUsage>,
) -> ApiConversationUsageMetadata {
    ApiConversationUsageMetadata {
        context_window_usage,
        summarized,
        credits_spent,
        #[allow(deprecated)]
        token_usage,
        tool_usage_metadata,
        warp_token_usage: labrador_token_usage,
        byok_token_usage,
    }
}

pub(crate) fn empty_conversation_usage_metadata() -> ApiConversationUsageMetadata {
    conversation_usage_metadata(
        0.0,
        false,
        0.0,
        vec![],
        None,
        Default::default(),
        Default::default(),
    )
}

pub(crate) fn take_labrador_token_usage(
    usage_metadata: &mut ApiConversationUsageMetadata,
) -> HashMap<String, ApiModelTokenUsage> {
    std::mem::take(&mut usage_metadata.warp_token_usage)
}

/// Unique, server-generated conversation-scoped token to be roundtripped to the API when sending
/// requests that follow-up within a given conversation.
#[derive(Serialize, Debug, Clone, PartialEq, Eq, Hash)]
pub struct ServerConversationToken(String);

impl ServerConversationToken {
    pub fn new(id: String) -> Self {
        Self(id)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn debug_link(&self) -> String {
        format!(
            "{}/debug/maa/{}",
            ChannelState::server_root_url(),
            self.as_str()
        )
    }

    pub fn conversation_link(&self) -> String {
        format!(
            "{}/conversation/{}",
            ChannelState::server_root_url(),
            self.as_str()
        )
    }
}

impl From<ServerConversationToken> for String {
    fn from(value: ServerConversationToken) -> Self {
        value.0
    }
}

// Conversions between AI ServerConversationToken and protocol ServerConversationToken
impl From<session_sharing_protocol::common::ServerConversationToken> for ServerConversationToken {
    fn from(token: session_sharing_protocol::common::ServerConversationToken) -> Self {
        Self(token.to_string())
    }
}

impl TryFrom<ServerConversationToken>
    for session_sharing_protocol::common::ServerConversationToken
{
    type Error = uuid::Error;

    fn try_from(token: ServerConversationToken) -> Result<Self, Self::Error> {
        token.as_str().parse()
    }
}

#[derive(Debug, Clone)]
pub struct RequestParams {
    pub input: Vec<AIAgentInput>,
    pub conversation_token: Option<ServerConversationToken>,
    pub forked_from_conversation_token: Option<ServerConversationToken>,
    /// Legacy ambient agent task linkage. Always None in this fork; kept for the
    /// outgoing request metadata until the field is removed server-side.
    pub ambient_agent_task_id: Option<crate::ai::agent_sdk::AmbientAgentTaskId>,
    pub tasks: Vec<labrador_multi_agent_api::Task>,
    pub existing_suggestions: Option<Suggestions>,
    pub metadata: Option<RequestMetadata>,
    pub session_context: SessionContext,
    pub model: LLMId,
    #[allow(unused)]
    pub coding_model: LLMId,
    pub cli_agent_model: LLMId,
    pub computer_use_model: LLMId,
    pub is_memory_enabled: bool,
    pub labrador_drive_context_enabled: bool,
    pub context_window_limit: Option<u32>,
    pub mcp_context: Option<MCPContext>,
    pub planning_enabled: bool,
    should_redact_secrets: bool,

    /// User-provided API keys for AI providers (BYO API Key).
    pub api_keys: Option<labrador_multi_agent_api::request::settings::ApiKeys>,
    pub allow_use_of_labrador_credits_with_byok: bool,
    pub autonomy_level: labrador_multi_agent_api::AutonomyLevel,
    pub isolation_level: labrador_multi_agent_api::IsolationLevel,
    pub web_search_enabled: bool,
    pub computer_use_enabled: bool,
    pub ask_user_question_enabled: bool,
    pub research_agent_enabled: bool,
    pub orchestration_enabled: bool,
    pub supported_tools_override: Option<Vec<labrador_multi_agent_api::ToolType>>,
    /// The conversation ID of the parent agent that spawned this child agent, if any.
    pub parent_agent_id: Option<String>,
    /// The display name for this agent (e.g. "Agent 1"), assigned by the orchestrator.
    pub agent_name: Option<String>,
}

pub type Event = Result<labrador_multi_agent_api::ResponseEvent, Arc<AIApiError>>;

#[cfg(not(target_family = "wasm"))]
pub type ResponseStream = Pin<Box<dyn Stream<Item = Event> + Send + 'static>>;

// The WASM version of this type has no bound on `Send`, which is an unnecessary bound when
// targeting wasm because the browser is single-threaded (and we don't leverage WebWorkers for async
// execution in Labrador Web).
#[cfg(target_family = "wasm")]
pub type ResponseStream = Pin<Box<dyn Stream<Item = Event>>>;

#[derive(Debug, Clone)]
pub struct ConversationData {
    pub id: AIConversationId,
    pub tasks: Vec<labrador_multi_agent_api::Task>,
    pub server_conversation_token: Option<ServerConversationToken>,
    pub forked_from_conversation_token: Option<ServerConversationToken>,
    pub existing_suggestions: Option<Suggestions>,
}

impl RequestParams {
    pub fn new(
        terminal_view_id: Option<EntityId>,
        session_context: SessionContext,
        request_input: &RequestInput,
        conversation: ConversationData,
        metadata: Option<RequestMetadata>,
        app: &AppContext,
    ) -> Self {
        let ai_settings = AISettings::as_ref(app);
        let is_memory_enabled = ai_settings.is_memory_enabled(app);
        let labrador_drive_context_enabled = ai_settings.is_labrador_drive_context_enabled(app);

        // Build MCP context - either grouped by server or flat lists based on feature flag
        let mcp_context = if FeatureFlag::MCPGroupedServerContext.is_enabled() {
            // Group MCP tools and resources by server
            let templatable_manager = TemplatableMCPServerManager::as_ref(app);

            let mut active_servers: Vec<&TemplatableMCPServerInfo> = templatable_manager
                .get_active_templatable_servers()
                .values()
                .copied()
                .collect();

            // If file-based MCP servers are enabled, add active servers in scope of
            // the user's current working directory
            if let Some(cwd) = session_context.current_working_directory() {
                active_servers.extend(
                    templatable_manager
                        .get_active_file_based_servers(Path::new(cwd), app)
                        .values(),
                );
            }

            let servers: Vec<MCPServer> = active_servers
                .into_iter()
                .map(|server| MCPServer {
                    name: server.name().to_string(),
                    description: server.description().unwrap_or_default().to_string(),
                    id: server.installation_id().to_string(),
                    resources: server.resources().to_vec(),
                    tools: server.tools().to_vec(),
                })
                .collect();

            if servers.is_empty() {
                None
            } else {
                #[allow(deprecated)]
                Some(MCPContext {
                    resources: vec![],
                    tools: vec![],
                    servers,
                })
            }
        } else {
            // Flat lists of resources and tools
            let templatable_mcp_manager = TemplatableMCPServerManager::as_ref(app);
            let resources = templatable_mcp_manager
                .resources()
                .cloned()
                .collect::<Vec<_>>();
            let tools = templatable_mcp_manager.tools().cloned().collect::<Vec<_>>();

            #[allow(deprecated)]
            (!resources.is_empty() || !tools.is_empty()).then_some(MCPContext {
                resources,
                tools,
                servers: vec![],
            })
        };

        let should_redact_secrets = get_secret_obfuscation_mode(app).should_redact_secret();

        let user_workspaces = UserWorkspaces::as_ref(app);
        let api_keys = ApiKeyManager::as_ref(app)
            .api_keys_for_request(user_workspaces.is_byo_api_key_enabled(), false);
        let allow_use_of_labrador_credits_with_byok = *AISettings::as_ref(app)
            .can_use_labrador_credits_with_byok
            && !ai::local_openai_auth::has_access_token()
            && !ai::local_claude_auth::has_auth_state();

        let app_execution_mode = AppExecutionMode::as_ref(app);
        let autonomy_level = if app_execution_mode.is_autonomous() {
            labrador_multi_agent_api::AutonomyLevel::Unsupervised
        } else {
            labrador_multi_agent_api::AutonomyLevel::Supervised
        };

        let isolation_level = if app_execution_mode.is_sandboxed() {
            labrador_multi_agent_api::IsolationLevel::Sandbox
        } else {
            labrador_multi_agent_api::IsolationLevel::None
        };

        let web_search_enabled =
            BlocklistAIPermissions::as_ref(app).get_web_search_enabled(app, terminal_view_id);
        let research_agent_enabled = app
            .private_user_preferences()
            .read_value("ResearchAgentEnabled")
            .ok()
            .flatten()
            .and_then(|s| s.parse().ok())
            .unwrap_or_default();
        let is_ambient_agent = false;
        let computer_use_enabled = FeatureFlag::AgentModeComputerUse.is_enabled()
            && BlocklistAIPermissions::as_ref(app)
                .get_computer_use_setting(app, terminal_view_id)
                .is_enabled()
            && computer_use::is_supported_on_current_platform()
            && (FeatureFlag::LocalComputerUse.is_enabled() || is_ambient_agent);
        let ask_user_question_enabled = BlocklistAIPermissions::as_ref(app)
            .get_ask_user_question_setting(app, terminal_view_id)
            != crate::ai::execution_profiles::AskUserQuestionPermission::Never;

        let orchestration_enabled = ai_settings.is_orchestration_enabled(app)
            && session_context
                .session_type()
                .as_ref()
                .is_none_or(|t| matches!(t, crate::terminal::model::session::SessionType::Local));

        // Reconcile the persisted override against the active base model's
        // current `LLMContextWindow` instead of trusting whatever was stored
        // last. If the active model isn't configurable or has been removed
        // server-side, drop the override; otherwise clamp it to the model's
        // current `[min, max]` range. This closes the window between an
        // in-flight model metadata refresh and the next request.
        let context_window_limit = {
            let profile_data = AIExecutionProfilesModel::as_ref(app)
                .active_profile(terminal_view_id, app)
                .data()
                .clone();
            profile_data
                .configurable_context_window(app)
                .and_then(|cw| {
                    profile_data
                        .context_window_limit
                        .map(|v| v.clamp(cw.min, cw.max))
                })
        };

        Self {
            input: request_input.all_inputs().cloned().collect(),
            conversation_token: conversation.server_conversation_token,
            forked_from_conversation_token: conversation.forked_from_conversation_token,
            ambient_agent_task_id: None,
            tasks: conversation.tasks,
            existing_suggestions: conversation.existing_suggestions,
            context_window_limit,
            metadata,
            session_context,
            model: request_input.model_id.clone(),
            coding_model: request_input.coding_model_id.clone(),
            cli_agent_model: request_input.cli_agent_model_id.clone(),
            computer_use_model: request_input.computer_use_model_id.clone(),
            is_memory_enabled,
            labrador_drive_context_enabled,
            mcp_context,
            planning_enabled: true,
            should_redact_secrets,
            api_keys,
            allow_use_of_labrador_credits_with_byok,
            autonomy_level,
            isolation_level,
            web_search_enabled,
            computer_use_enabled,
            ask_user_question_enabled,
            research_agent_enabled,
            orchestration_enabled,
            supported_tools_override: request_input.supported_tools_override.clone(),
            parent_agent_id: None,
            agent_name: None,
        }
    }
}
