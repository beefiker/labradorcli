use std::{
    borrow::Cow,
    collections::HashMap,
    ffi::OsString,
    future::Future,
    io,
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    thread,
    time::Duration,
};

use crate::ai::blocklist::task_status_sync_model::TaskStatusSyncModel;
use crate::ai::agent_sdk::driver::harness::{
    task_env_vars, HarnessKind, HarnessRunner, ResumePayload, SavePoint, ThirdPartyHarness,
};
use crate::terminal::cli_agent_sessions::plugin_manager::{
    plugin_manager_for, CliAgentPluginManager,
};
use crate::terminal::cli_agent_sessions::{
    CLIAgentSessionStatus, CLIAgentSessionsModel, CLIAgentSessionsModelEvent,
};
use crate::{
    ai::{
        agent::{CancellationReason, RenderableAIError},
        ambient_agents::AmbientAgentTaskId,
        cloud_environments::{AmbientAgentEnvironment, CloudAmbientAgentEnvironment},
    },
    auth::AuthStateProvider,
    cloud_object::CloudObject,
    server::server_api::{
        ai::AIClient,
        harness_support::{
            HarnessSupportClient, ResolvePromptAttachedSkill, ResolvePromptRequest,
        },
        ServerApiProvider,
    },
    terminal::view::ConversationRestorationInNewPaneType,
};
use ai::skills::ParsedSkill;
use anyhow::{anyhow, Context as _};
use futures::{channel::oneshot, FutureExt as _};
use oneshot::Canceled;
use warp_cli::agent::{Harness, OutputFormat};
use warp_cli::share::ShareRequest;
use warp_core::{
    channel::{Channel, ChannelState},
    features::FeatureFlag,
    report_error, report_if_error, safe_debug, safe_info,
};
use warp_graphql::ai::AgentTaskState;
use warp_managed_secrets::ManagedSecretValue;
use warpui::{
    r#async::{FutureExt, TimeoutError},
    AppContext, Entity, ModelContext, ModelHandle, ModelSpawner, SingletonEntity,
};

pub(crate) mod attachments;
pub(crate) mod environment;
mod error_classification;
pub(crate) mod harness;
pub(super) mod output;
mod snapshot;
pub(crate) mod terminal;

use environment::PrepareEnvironmentError;
use terminal::TerminalDriverEvent;

const HARNESS_SAVE_INTERVAL: Duration = Duration::from_secs(30);
pub(crate) const WARP_DRIVE_SYNC_TIMEOUT: Duration = Duration::from_secs(60);
const SETUP_FAILED_IDLE_TIMEOUT: Duration = Duration::from_secs(120);
/// Signals to Claude child-harness hooks that Warp already owns the background
/// message-listener lifecycle, so the plugin should reuse the shared state
/// files instead of spawning and cleaning up its own listener.
///
/// When this variable is absent, the Claude plugin falls back to its legacy
/// self-managed listener path so older Warp builds and standalone plugin
/// invocations keep working.
pub(crate) const OZ_MESSAGE_LISTENER_MANAGED_EXTERNALLY_ENV: &str =
    "OZ_MESSAGE_LISTENER_MANAGED_EXTERNALLY";
/// Optional root directory for the per-session Claude message-listener state
/// that Warp and the Claude hook scripts share.
pub(crate) const OZ_MESSAGE_LISTENER_STATE_ROOT_ENV: &str = "OZ_MESSAGE_LISTENER_STATE_ROOT";
// Keep exporting the legacy `OZ_PARENT_*` names to child hooks until the
// external Claude plugin has fully migrated to the canonical
// `OZ_MESSAGE_LISTENER_*` names.
const LEGACY_OZ_PARENT_LISTENER_MANAGED_EXTERNALLY_ENV: &str =
    "OZ_PARENT_LISTENER_MANAGED_EXTERNALLY";
const LEGACY_OZ_PARENT_STATE_ROOT_ENV: &str = "OZ_PARENT_STATE_ROOT";

/// IdleTimeoutSender is wrapper around a sender that signals when a run is done after
/// an idle timeout. Used for both Oz runs and third-party harnesses.
///
/// We use a generation-based approach to cancel timers instead of storing timer handles:
///
/// - `tx_cell` holds the completion sender; taking it ensures we only complete once.
/// - `timer_generation` starts at 0 and is incremented each time we want to cancel
///   existing timers and potentially start a new one. When a timer fires, it checks
///   if its generation still matches the current generation. If not, the timer was
///   "cancelled" by a newer timer and should not complete the conversation.
///
/// This approach avoids the complexity of storing and cancelling timer handles,
/// while allowing multiple events to safely race without double-completion.
struct IdleTimeoutSender<T: Send + 'static> {
    tx_cell: Arc<Mutex<Option<oneshot::Sender<T>>>>,
    generation: Arc<AtomicUsize>,
}

impl<T: Send + 'static> IdleTimeoutSender<T> {
    fn new(tx: oneshot::Sender<T>) -> Self {
        Self {
            tx_cell: Arc::new(Mutex::new(Some(tx))),
            generation: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// End the run by sending `value` immediately.
    fn end_run_now(&self, value: T) {
        if let Ok(mut guard) = self.tx_cell.lock() {
            if let Some(sender) = guard.take() {
                let _ = sender.send(value);
            }
        }
    }

    /// End the run after `timeout` by sending `value`, unless cancelled before then.
    fn end_run_after(&self, timeout: Duration, value: T) {
        // Increment the generation counter to invalidate any existing timers,
        // then capture the new generation for our timer to check against.
        let current_gen = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let tx_cell = Arc::clone(&self.tx_cell);
        let generation = Arc::clone(&self.generation);

        // Spawn a background thread that will complete the oneshot after the idle timeout,
        // unless a follow-up query resets the timer (by bumping the generation counter).
        thread::spawn(move || {
            thread::sleep(timeout);

            // Check if our timer generation is still current. If not, a follow-up
            // query or other activity has "cancelled" this timer by bumping the generation.
            if generation.load(Ordering::SeqCst) != current_gen {
                return;
            }
            if let Ok(mut guard) = tx_cell.lock() {
                if let Some(sender) = guard.take() {
                    // Send the value after the idle timeout expires.
                    let _ = sender.send(value);
                }
            }
        });
    }

    /// Cancel any pending idle timers.
    fn cancel_idle_timeout(&self) {
        if self.generation.load(Ordering::SeqCst) > 0 {
            self.generation.fetch_add(1, Ordering::SeqCst);
        }
    }
}

/// How to resume an existing conversation when starting an agent run.
///
/// Third-party harnesses round-trip a harness-specific payload
/// (see [`ResumePayload`]) — the previous Oz variant has been removed.
pub enum ResumeOptions {
    ThirdParty(Box<ResumePayload>),
}

/// Options for initializing the agent driver.
pub struct AgentDriverOptions {
    /// Initial working directory for the agent's terminal session.
    pub working_dir: PathBuf,
    /// Secrets to inject into the agent's terminal session.
    pub secrets: HashMap<String, ManagedSecretValue>,
    /// ID of the task being executed.
    pub task_id: Option<AmbientAgentTaskId>,
    /// Parent run ID for child orchestration flows, if this task was spawned by another run.
    pub parent_run_id: Option<String>,
    /// Whether the agent run should share its session.
    pub should_share: bool,
    /// How long to keep the session alive after the agent run completes, if at all.
    pub idle_on_complete: Option<Duration>,
    /// If set, resume an existing conversation instead of starting fresh. The variant
    /// determines which harness-specific path is taken (Oz transcript restore vs.
    /// third-party-harness payload rehydration).
    pub resume: Option<ResumeOptions>,
    /// Resolved environment configuration, if any.
    pub environment: Option<AmbientAgentEnvironment>,
    /// Selected execution harness for this run.
    pub selected_harness: Harness,
    /// Whether to skip end-of-run snapshot upload.
    pub snapshot_disabled: Option<bool>,
    /// End-of-run snapshot upload timeout override.
    pub snapshot_upload_timeout: Option<Duration>,
    /// Declarations script timeout override.
    pub snapshot_script_timeout: Option<Duration>,
}

/// `AgentDriver` is a model for driving an ambient Dwarf agent to completion.
///
/// Its primary responsibility is to configure a headless terminal pane and execute an AI query within it.
pub struct AgentDriver {
    terminal_driver: ModelHandle<terminal::TerminalDriver>,
    working_dir: PathBuf,

    /// Secrets available to the running agent.
    /// - Secrets are injected as environment variables when the terminal session is created.
    /// - Secrets are passed to MCP servers during spawning.
    secrets: Arc<HashMap<String, ManagedSecretValue>>,

    output_format: OutputFormat,

    // The associated task ID for this agent run, if any.
    task_id: Option<AmbientAgentTaskId>,

    /// Harness adapter for the running agent. This is only set if:
    /// - The harness has started successfully.
    /// - We're using a third-party harness.
    /// In the future, we _may_ use the harness abstraction for the Oz agent as well.
    harness: Option<Arc<dyn HarnessRunner>>,

    // Optional idle timeout after completion. If set, the process will stay alive for follow-ups
    // and exit after this period of inactivity.
    idle_on_complete: Option<Duration>,

    /// Resolved environment configuration.
    environment: Option<AmbientAgentEnvironment>,

    // End-of-run snapshot upload controls.
    snapshot_disabled: bool,
    snapshot_upload_timeout: Duration,
    snapshot_script_timeout: Duration,

    /// If set, a third-party-harness conversation to resume. Consumed by `prepare_harness`
    /// when building the runner and taken back to `None` after use so subsequent runs start
    /// fresh.
    resume_payload: Option<ResumePayload>,
}

/// Task configuration for running an agent.
#[derive(Debug)]
pub struct Task {
    /// The prompt for the agent.
    pub prompt: AgentRunPrompt,
    /// Which harness to use for executing the agent run.
    pub harness: HarnessKind,
}

/// Prompt that we initialize an agent driver with. Can represent either a local prompt or
/// a prompt that we resolve server-side.
#[derive(Debug, Clone)]
pub enum AgentRunPrompt {
    /// Prompt is provided locally (already resolved to a plain string).
    Local(String),
    /// Server resolves prompt from the task's stored prompt.
    /// Used when task_id is provided without an explicit prompt.
    ServerSide {
        /// Optional skill whose instructions are sent to the agent.
        skill: Option<ParsedSkill>,
        /// Directory where task attachments were downloaded.
        attachments_dir: Option<String>,
    },
}

#[derive(Debug, thiserror::Error)]
#[allow(dead_code)] // some MCP* / cloud-env-related variants are only constructed by deleted Oz paths
pub enum AgentDriverError {
    #[error("Terminal session is not available.")]
    TerminalUnavailable,
    #[error("Invalid runtime state - please file a bug report.")]
    InvalidRuntimeState,
    #[error("Requested MCP server not found: {0}")]
    MCPServerNotFound(uuid::Uuid),
    #[error("Failed to start MCP servers")]
    MCPStartupFailed,
    #[error("Failed to parse MCP server JSON: {0}")]
    MCPJsonParseError(String),
    #[error("MCP server configuration is missing required variables")]
    MCPMissingVariables,
    #[error("Agent profile \"{0}\" not found")]
    ProfileError(String),
    #[error(
        "Failed to authenticate with server - please log in via 'oz login', provide an API key via '--api-key <key>', or set the WARP_API_KEY environment variable"
    )]
    NotLoggedIn,
    #[error("Saved prompt not found for id {0}")]
    AIWorkflowNotFound(String),
    #[error("Terminal bootstrap failed")]
    BootstrapFailed,
    #[error("Unable to share agent session")]
    ShareSessionFailed {
        #[source]
        error: terminal::ShareSessionError,
    },
    #[error("Error syncing Dwarf Drive")]
    WarpDriveSyncFailed,
    #[error("Requested environment not found: {0}")]
    EnvironmentNotFound(String),
    #[error("Environment setup failed: {0}")]
    EnvironmentSetupFailed(String),
    #[error("Could not resolve working directory {}", path.display())]
    InvalidWorkingDirectory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{error}")]
    ConversationError { error: RenderableAIError },
    #[error("Conversation was canceled: {reason}")]
    ConversationCancelled { reason: CancellationReason },
    #[error("The agent got stuck waiting for user confirmation on the action: {blocked_action}")]
    ConversationBlocked { blocked_action: String },
    #[error("Timed out refreshing team metadata")]
    TeamMetadataRefreshTimeout,
    #[error("{0}")]
    SkillResolutionFailed(String),
    #[error("Failed to build agent configuration")]
    ConfigBuildFailed(#[source] anyhow::Error),
    #[error("Failed to resolve server-side prompt")]
    PromptResolutionFailed(#[source] anyhow::Error),
    #[error("Failed to fetch task secrets")]
    SecretsFetchFailed(#[source] anyhow::Error),
    #[error("Failed to load conversation: {0}")]
    ConversationLoadFailed(String),
    #[error("Failed to initialize AWS Bedrock credentials: {0}")]
    AwsBedrockCredentialsFailed(String),
    #[error(
        "Conversation {conversation_id} was produced by the {expected} harness, but --harness {got} was requested. \
         Re-run with --harness {expected} (or omit --harness to match) to continue this conversation."
    )]
    ConversationHarnessMismatch {
        conversation_id: String,
        expected: String,
        got: String,
    },
    #[error(
        "Task {task_id} was created with the {expected} harness, but --harness {got} was requested. \
         Re-run with --harness {expected} (or omit --harness to match) to continue this task."
    )]
    TaskHarnessMismatch {
        task_id: String,
        expected: String,
        got: String,
    },
    #[error(
        "Conversation {conversation_id} has no stored transcript for the {harness} harness. \
         The prior run may have crashed before saving any state."
    )]
    ConversationResumeStateMissing {
        harness: String,
        conversation_id: String,
    },
    #[error("Harness command exited with code {exit_code}")]
    HarnessCommandFailed { exit_code: i32 },
    #[error("Harness '{harness}' setup failed: {reason}")]
    HarnessSetupFailed { harness: String, reason: String },
    #[error("Harness '{harness}' config setup failed")]
    HarnessConfigSetupFailed {
        harness: String,
        #[source]
        error: anyhow::Error,
    },
}

impl From<warpui::ModelDropped> for AgentDriverError {
    fn from(_: warpui::ModelDropped) -> Self {
        AgentDriverError::InvalidRuntimeState
    }
}

impl From<PrepareEnvironmentError> for AgentDriverError {
    fn from(error: PrepareEnvironmentError) -> Self {
        match error {
            PrepareEnvironmentError::InvalidRuntimeState => AgentDriverError::InvalidRuntimeState,
            PrepareEnvironmentError::TerminalDriver { source } => source,
            error => AgentDriverError::EnvironmentSetupFailed(error.to_string()),
        }
    }
}

impl AgentDriver {
    pub fn new(
        options: AgentDriverOptions,
        ctx: &mut ModelContext<Self>,
    ) -> Result<Self, AgentDriverError> {
        let AgentDriverOptions {
            working_dir,
            task_id,
            parent_run_id,
            should_share,
            idle_on_complete,
            secrets,
            resume,
            environment,
            selected_harness,
            snapshot_disabled,
            snapshot_upload_timeout,
            snapshot_script_timeout,
        } = options;

        let (conversation_restoration, resume_payload): (
            Option<ConversationRestorationInNewPaneType>,
            Option<ResumePayload>,
        ) = match resume {
            Some(ResumeOptions::ThirdParty(payload)) => (None, Some(*payload)),
            None => (None, None),
        };

        safe_info!(
            safe: ("Initializing agent driver: share={should_share}, idle_on_complete={idle_on_complete:?}"),
            full: (
                "Initializing agent driver: share={should_share}, idle_on_complete={idle_on_complete:?}, working_dir={}",
                working_dir.display()
            )
        );

        // If we're not logged in, the root view will go to an auth screen, and all subsequent steps will fail.
        // This should be impossible, since we enforce login before reaching this point.
        if !matches!(ChannelState::channel(), Channel::Oss)
            && !AuthStateProvider::as_ref(ctx).get().is_logged_in()
        {
            return Err(AgentDriverError::NotLoggedIn);
        }

        // Build environment variables from secrets for the terminal session.
        // Do not override env vars that are already set to a non-empty value in the current
        // process. This ensures that worker-injected credentials (e.g. harness auth secrets)
        // and user-provided env vars (e.g. on self-hosted workers) take precedence over
        // generic managed secrets.
        let mut env_vars = HashMap::with_capacity(secrets.len() + 1);
        for (name, secret) in &secrets {
            let (env_name, env_value) = match secret {
                ManagedSecretValue::RawValue { value } => (name.as_str(), value.as_str()),
                ManagedSecretValue::AnthropicApiKey { api_key } => {
                    ("ANTHROPIC_API_KEY", api_key.as_str())
                }
                ManagedSecretValue::AnthropicBedrockAccessKey {
                    aws_access_key_id,
                    aws_secret_access_key,
                    aws_session_token,
                    aws_region,
                } => {
                    // Inject env vars needed for Claude Code Bedrock access key authentication.
                    // AWS_SESSION_TOKEN is only injected when the user provided one (i.e. for
                    // temporary/STS credentials).
                    let mut vars = vec![
                        ("AWS_ACCESS_KEY_ID", aws_access_key_id.as_str()),
                        ("AWS_SECRET_ACCESS_KEY", aws_secret_access_key.as_str()),
                        ("CLAUDE_CODE_USE_BEDROCK", "1"),
                        ("AWS_REGION", aws_region.as_str()),
                    ];
                    if let Some(token) = aws_session_token.as_deref() {
                        vars.push(("AWS_SESSION_TOKEN", token));
                    }
                    for (env_name, env_value) in vars {
                        if std::env::var(env_name).is_ok_and(|v| !v.is_empty()) {
                            log::warn!(
                                "Skipping managed secret {env_name}: already set in environment"
                            );
                            continue;
                        }
                        env_vars.insert(OsString::from(env_name), OsString::from(env_value));
                    }
                    continue; // Skip the single-var insert below since we handled all vars inline.
                }
                ManagedSecretValue::AnthropicBedrockApiKey {
                    aws_bearer_token_bedrock,
                    aws_region,
                } => {
                    // Inject all three env vars needed for Claude Code Bedrock authentication.
                    let vars = [
                        (
                            "AWS_BEARER_TOKEN_BEDROCK",
                            aws_bearer_token_bedrock.as_str(),
                        ),
                        ("CLAUDE_CODE_USE_BEDROCK", "1"),
                        ("AWS_REGION", aws_region.as_str()),
                    ];
                    for (env_name, env_value) in vars {
                        if std::env::var(env_name).is_ok_and(|v| !v.is_empty()) {
                            log::warn!(
                                "Skipping managed secret {env_name}: already set in environment"
                            );
                            continue;
                        }
                        env_vars.insert(OsString::from(env_name), OsString::from(env_value));
                    }
                    continue; // Skip the single-var insert below since we handled all vars inline.
                }
            };
            if std::env::var(env_name).is_ok_and(|v| !v.is_empty()) {
                log::warn!("Skipping managed secret {env_name}: already set in environment");
                continue;
            }
            env_vars.insert(OsString::from(env_name), OsString::from(env_value));
        }

        env_vars.extend(task_env_vars(
            task_id.as_ref(),
            parent_run_id.as_deref(),
            selected_harness,
        ));

        // Signal to third-party harnesses (e.g. Claude Code) that we're in a sandbox
        // so they allow root execution with permissive flags.
        if warp_isolation_platform::detect().is_some() {
            env_vars.insert(OsString::from("IS_SANDBOX"), OsString::from("1"));
        }

        let terminal_driver = terminal::TerminalDriver::create(
            terminal::TerminalDriverOptions {
                working_dir: working_dir.clone(),
                env_vars,
                should_share,
                task_id,
                conversation_restoration,
            },
            ctx,
        )?;

        // Subscribe to TerminalDriver events for task-specific handling.
        ctx.subscribe_to_model(&terminal_driver, |me, event, ctx| {
            me.handle_terminal_driver_event(event, ctx);
        });

        Ok(Self {
            terminal_driver,
            working_dir,
            secrets: Arc::new(secrets),
            output_format: OutputFormat::default(),
            task_id,
            harness: None,
            idle_on_complete,
            environment,
            snapshot_disabled: snapshot_disabled.unwrap_or(false),
            snapshot_upload_timeout: snapshot_upload_timeout
                .unwrap_or(snapshot::DEFAULT_SNAPSHOT_UPLOAD_TIMEOUT),
            snapshot_script_timeout: snapshot_script_timeout
                .unwrap_or(snapshot::DEFAULT_DECLARATIONS_SCRIPT_TIMEOUT),
            resume_payload,
        })
    }

    pub fn set_output_format(&mut self, output_format: OutputFormat) {
        self.output_format = output_format;
    }

    pub fn add_share_requests(
        &self,
        share_requests: impl IntoIterator<Item = ShareRequest>,
        ctx: &mut ModelContext<Self>,
    ) {
        self.terminal_driver.update(ctx, |td, ctx| {
            td.add_share_requests(share_requests, ctx);
        });
    }

    pub fn run(
        &mut self,
        task: Task,
        ctx: &mut ModelContext<Self>,
    ) -> impl Future<Output = Result<(), AgentDriverError>> {
        let (tx, rx) = oneshot::channel();
        let foreground = ctx.spawner();
        let server_api = ServerApiProvider::as_ref(ctx).get_ai_client();
        let task_id = self.task_id;
        let idle_on_complete = self.idle_on_complete;

        ctx.spawn(
            async move {
                // Mark the task as IN_PROGRESS before starting work. This covers
                // the gap during environment setup, MCP startup, etc. — before any
                // conversation exists and TaskStatusSyncModel can fire.
                if let Some(task_id) = task_id {
                    if let Err(e) = server_api
                        .update_agent_task(
                            task_id,
                            Some(AgentTaskState::InProgress),
                            None,
                            None,
                            None,
                        )
                        .await
                    {
                        log::error!("Failed to update agent task state to InProgress: {e}");
                    }
                }
                let result = Self::run_internal(task, foreground.clone()).await;

                // Run the snapshot upload before signaling the caller. The caller resumes and
                // triggers process termination as soon as it receives `result`; the snapshot
                // upload depends on the event loop that termination tears down, so anything
                // async it awaits (presigned URL fetch, uploads, timers) would get abandoned
                // mid-flight. Provider cleanup is just local temp-file teardown, so it's safe
                // to run after the send.
                Self::run_snapshot_upload(&foreground).await;

                if tx.send(result).is_err() {
                    log::error!("Caller did not wait for agent driver to finish");
                }

                Self::cleanup(foreground).await;
            },
            |_, _, _| {},
        );

        let server_api_for_error = ServerApiProvider::as_ref(ctx).get_ai_client();

        async move {
            if let Some(ref task_id) = task_id {
                log::info!("Executing task {task_id}");
            }

            let result = match rx.await {
                Ok(result) => result,
                Err(Canceled) => {
                    log::error!("Agent driver exited abruptly");
                    Err(AgentDriverError::InvalidRuntimeState)
                }
            };

            // Report driver-level errors directly to the server. These errors
            // occur before or outside a conversation (e.g. bootstrap, MCP startup,
            // environment setup) so TaskStatusSyncModel never fires for them.
            // Success/blocked/cancelled are handled by TaskStatusSyncModel.
            if let (Some(task_id), Err(err)) = (task_id, &result) {
                report_driver_error(task_id, err, &server_api_for_error).await;

                // Keep the session alive after environment setup failures so
                // the viewer can connect, receive scrollback, and see the error.
                if let (Some(idle_timeout), true) = (
                    idle_on_complete,
                    matches!(err, AgentDriverError::EnvironmentSetupFailed(_)),
                ) {
                    let timeout = idle_timeout.min(SETUP_FAILED_IDLE_TIMEOUT);
                    log::info!("Environment setup failed; keeping session alive for {timeout:?}");
                    warpui::r#async::Timer::after(timeout).await;
                }
            }

            result
        }
    }

    /// Log all valid environment IDs for the user.
    pub(super) fn log_valid_environments(app: &AppContext) {
        let environments = CloudAmbientAgentEnvironment::get_all(app);
        if environments.is_empty() {
            log::error!("No environments available for this user.");
        } else {
            log::error!("Valid environment IDs:");
            for env in environments {
                log::error!("  - {} ({})", env.sync_id(), env.model().string_model.name);
            }
        }
    }

    /// Check that the working directory exists. Since it's user-specified, we don't automatically
    /// create the directory (in case they made a typo).
    fn check_working_dir(&self) -> impl Future<Output = Result<(), AgentDriverError>> {
        let working_dir = self.working_dir.clone();
        async move {
            match async_fs::metadata(&working_dir).await {
                Ok(metadata) => {
                    if metadata.is_dir() {
                        Ok(())
                    } else {
                        Err(AgentDriverError::InvalidWorkingDirectory {
                            path: working_dir.to_owned(),
                            source: io::ErrorKind::NotADirectory.into(),
                        })
                    }
                }
                Err(err) => Err(AgentDriverError::InvalidWorkingDirectory {
                    path: working_dir.to_owned(),
                    source: err,
                }),
            }
        }
    }

    /// Runs the agent to completion.
    /// Driving the agent mostly requires main-thread UI framework updates, but using `async` and
    /// a `ModelSpawner` lets us express the high-level process linearly rather than in a
    /// series of callbacks and state machine updates.
    async fn run_internal(
        task: Task,
        foreground: ModelSpawner<Self>,
    ) -> Result<(), AgentDriverError> {
        safe_debug!(
            safe: ("Running agent driver"),
            full: ("Running agent driver for query `{:?}`", task.prompt)
        );

        foreground
            .spawn(|me, _| me.check_working_dir())
            .await?
            .await?;

        // IMPORTANT: Wait for the terminal session to bootstrap before starting MCP servers.
        // Some of the initializations are necessary for the MCP servers to start correctly.
        //
        // Why: MCP server startup can happen before we actually execute the agent prompt. For
        // `TransportType::CLIServer` MCPs we currently depend on `AISettings.mcp_execution_path`,
        // which is populated as part of terminal bootstrap. Waiting for the session bootstrap
        // here avoids a subtle race where MCP spawn runs with an unset PATH and then the driver
        // only fails via a timeout.
        foreground
            .spawn(|me, ctx| {
                me.terminal_driver
                    .as_ref(ctx)
                    .wait_for_session_bootstrapped()
            })
            .await?
            .await?;

        // For all harnesses: wait for the shared session and prepare the environment.
        foreground
            .spawn(|me, ctx| {
                me.terminal_driver
                    .update(ctx, |driver, _| driver.wait_for_session_shared())
            })
            .await?
            .await?;

        let environment_opt = foreground.spawn(|me, _| me.environment.clone()).await?;

        if let Some(environment) = environment_opt {
            log::info!("Loading environment...");

            let harness = task.harness.harness();
            foreground
                .spawn(move |me, ctx| {
                    let working_dir = me.working_dir.clone();
                    me.terminal_driver.update(ctx, |_, ctx| {
                        environment::prepare_environment(
                            environment,
                            working_dir,
                            false, /* is_sandbox */
                            harness,
                            ctx,
                        )
                    })
                })
                .await?
                .await
                .map_err(AgentDriverError::from)?;

        }

        // Run the harness with a prompt
        match task.harness {
            HarnessKind::ThirdParty(harness) => {
                let harness_exit_rx = Self::setup_harness(harness.as_ref(), &foreground).await?;
                let runner =
                    Self::prepare_harness(&task.prompt, harness.as_ref(), &foreground).await?;
                Self::run_harness(runner, &foreground, harness_exit_rx).await
            }
            HarnessKind::Unsupported(harness) => Err(AgentDriverError::HarnessSetupFailed {
                harness: harness.to_string(),
                reason: format!(
                    "The {harness} harness is only supported for local child agent launches."
                ),
            }),
        }
    }

    /// Sets up the third-party harness by subscribing to CLI session events and
    /// installing the Dwarf plugin and platform plugin, if applicable.
    ///
    /// Returns a oneshot receiver that fires when the harness should exit
    /// (either immediately on completion or after the idle-on-complete timeout).
    async fn setup_harness(
        harness: &dyn ThirdPartyHarness,
        foreground: &ModelSpawner<Self>,
    ) -> Result<oneshot::Receiver<()>, AgentDriverError> {
        let (exit_tx, exit_rx) = oneshot::channel();
        let harness_exit = IdleTimeoutSender::new(exit_tx);

        // Subscribe to CLI agent session events so we can update the task
        // state as the harness emits stop/blocked notifications.
        foreground
            .spawn(move |me, ctx| me.subscribe_to_cli_agent_session_events(harness_exit, ctx))
            .await?;

        // Install plugins before running the harness command.
        let plugin_manager: Option<Box<dyn CliAgentPluginManager>> =
            plugin_manager_for(harness.cli_agent());
        if let Some(manager) = plugin_manager {
            if let Err(e) = manager.install().await {
                log::warn!("Plugin installation failed (continuing): {e}");
            }
            if let Err(e) = manager.install_platform_plugin().await {
                log::warn!("Platform plugin installation failed (continuing): {e}");
            }
        }

        Ok(exit_rx)
    }

    /// Configure a third-party harness for execution. This will set `self.harness` and
    /// return a handle to the harness runner.
    async fn prepare_harness(
        prompt: &AgentRunPrompt,
        harness: &dyn ThirdPartyHarness,
        foreground: &ModelSpawner<Self>,
    ) -> Result<Arc<dyn harness::HarnessRunner>, AgentDriverError> {
        let (working_dir, task_id, server_api, terminal_driver) = foreground
            .spawn(|me, ctx| {
                if me.harness.is_some() {
                    log::error!(
                        "Attempted to prepare a third-party harness, but one was already configured"
                    );
                    return Err(AgentDriverError::InvalidRuntimeState);
                }

                Ok((
                    me.working_dir.clone(),
                    me.task_id,
                    ServerApiProvider::as_ref(ctx).get(),
                    me.terminal_driver.clone(),
                ))
            })
            .await
            .map_err(|_| AgentDriverError::InvalidRuntimeState)
            .flatten()?;

        let (prompt_text, system_prompt, resumption_prompt): (
            Cow<'_, str>,
            Option<String>,
            Option<String>,
        ) = match prompt {
            AgentRunPrompt::Local(text) => (Cow::Borrowed(text), None, None),
            AgentRunPrompt::ServerSide {
                skill,
                attachments_dir,
            } => {
                let skill = skill
                    .as_ref()
                    .map(|parsed_skill| ResolvePromptAttachedSkill {
                        name: parsed_skill.name.clone(),
                        content: parsed_skill.content.clone(),
                        path: Some(parsed_skill.path.to_string_lossy().to_string()),
                    });
                let request = ResolvePromptRequest {
                    skill,
                    attachments_dir: attachments_dir.clone(),
                };
                let resolved = server_api
                    .resolve_prompt(request)
                    .await
                    .map_err(AgentDriverError::PromptResolutionFailed)?;
                (
                    Cow::Owned(resolved.prompt),
                    resolved.system_prompt,
                    resolved.resumption_prompt,
                )
            }
        };

        // Prepare harness config files (onboarding, trust dialog, API-key approval, etc.).
        let secrets = foreground
            .spawn(|me, _| Arc::clone(&me.secrets))
            .await
            .map_err(|_| AgentDriverError::InvalidRuntimeState)?;
        harness.prepare_environment_config(&working_dir, system_prompt.as_deref(), &secrets)?;

        // Pull the resume payload off the driver so the harness runner can rehydrate any
        // existing session/conversation state before launching its CLI. The payload variant
        // is harness-specific; harnesses match on their own [`ResumePayload`] variant and
        // ignore others.
        let resume = foreground
            .spawn(|me, _| me.resume_payload.take())
            .await
            .map_err(|_| AgentDriverError::InvalidRuntimeState)?;

        let runner: Arc<dyn HarnessRunner> = harness
            .build_runner(
                prompt_text.as_ref(),
                system_prompt.as_deref(),
                resumption_prompt.as_deref(),
                &working_dir,
                task_id,
                server_api,
                terminal_driver,
                resume,
            )?
            .into();

        let stored_runner = runner.clone();
        foreground
            .spawn(move |me, _| me.harness = Some(stored_runner))
            .await?;

        Ok(runner)
    }

    /// Execute a configured external harness in the terminal.
    ///
    /// The `harness_exit_rx` oneshot fires when the subscription determines it's
    /// time to exit (either immediately on completion or after the idle timeout).
    async fn run_harness(
        runner: Arc<dyn harness::HarnessRunner>,
        foreground: &ModelSpawner<Self>,
        harness_exit_rx: oneshot::Receiver<()>,
    ) -> Result<(), AgentDriverError> {
        // Start the third-party harness.
        let mut command_handle = runner.start(foreground).await?.fuse();
        let mut harness_exit_rx = harness_exit_rx.fuse();

        // Periodically save the conversation while the command is running and handle
        // exiting gracefully once the idle timeout elapses.
        let command_result = loop {
            futures::select! {
                exit_code = command_handle => break exit_code,
                _ = warpui::r#async::Timer::after(HARNESS_SAVE_INTERVAL).fuse() => {
                    log::debug!("Triggering periodic save of harness conversation data");
                    report_if_error!(runner
                        .save_conversation(SavePoint::Periodic, foreground)
                        .await
                        .context("Failed to save harness conversation (periodic)"));
                }
                _ = harness_exit_rx => {
                    log::debug!("Requesting harness exit");
                    report_if_error!(runner
                        .exit(foreground)
                        .await
                        .context("Failed to exit harness"));
                }
            }
        };

        // Final save after the command finishes.
        log::debug!("Triggering final save of harness conversation data");
        report_if_error!(runner
            .save_conversation(SavePoint::Final, foreground)
            .await
            .context("Failed to save harness conversation (final)"));
        report_if_error!(runner
            .cleanup(foreground)
            .await
            .context("Failed to clean up harness runtime state"));

        let exit_code = command_result?;
        log::debug!("Agent harness exited with status {exit_code}");

        if exit_code.was_successful() {
            Ok(())
        } else {
            Err(AgentDriverError::HarnessCommandFailed {
                exit_code: exit_code.value(),
            })
        }
    }

    /// Subscribe to the singleton `CLIAgentSessionsModel` so that idle-on-complete
    /// timers are driven by CLI agent session status changes.
    ///
    /// Task state reporting is handled centrally by `TaskStatusSyncModel`;
    /// the driver only registers the `terminal_view_id → task_id` mapping
    /// so that the sync model can look up the task for each session.
    fn subscribe_to_cli_agent_session_events(
        &self,
        harness_exit: IdleTimeoutSender<()>,
        ctx: &mut ModelContext<Self>,
    ) {
        let terminal_view_id = self.terminal_driver.as_ref(ctx).terminal_view().id();

        // Register this session with TaskStatusSyncModel so CLI agent
        // status changes are reported to the server.
        if let Some(task_id) = self.task_id {
            TaskStatusSyncModel::handle(ctx).update(ctx, |model, ctx| {
                model.register_cli_session(terminal_view_id, task_id, ctx);
            });
        }

        ctx.subscribe_to_model(
            &CLIAgentSessionsModel::handle(ctx),
            move |me, event, ctx| match event {
                CLIAgentSessionsModelEvent::StatusChanged {
                    terminal_view_id: event_tid,
                    status,
                    ..
                } => {
                    if *event_tid != terminal_view_id {
                        return;
                    }

                    // Drive idle-on-complete timer for the harness exit signal.
                    match status {
                        CLIAgentSessionStatus::Success | CLIAgentSessionStatus::Blocked { .. } => {
                            if let Some(idle_timeout) = me.idle_on_complete {
                                harness_exit.end_run_after(idle_timeout, ());
                            } else {
                                harness_exit.end_run_now(());
                            }
                        }
                        CLIAgentSessionStatus::InProgress => {
                            harness_exit.cancel_idle_timeout();
                        }
                    }
                }
                CLIAgentSessionsModelEvent::SessionUpdated {
                    terminal_view_id: event_tid,
                    ..
                } => {
                    if *event_tid != terminal_view_id {
                        return;
                    }

                    let Some(runner) = me.harness.clone() else {
                        return;
                    };
                    let spawner = ctx.spawner();
                    ctx.spawn(
                        async move {
                            log::debug!(
                                "Triggering post-turn harness session update from CLI agent event"
                            );
                            report_if_error!(runner
                                .handle_session_update(&spawner)
                                .await
                                .context("Failed to update harness state from CLI session event"));
                            log::debug!("Triggering post-turn save of harness conversation data");
                            report_if_error!(runner
                                .save_conversation(SavePoint::PostTurn, &spawner)
                                .await
                                .context("Failed to save harness conversation (post-turn)"));
                        },
                        |_, _, _| {},
                    );
                }
                CLIAgentSessionsModelEvent::Started { .. }
                | CLIAgentSessionsModelEvent::InputSessionChanged { .. }
                | CLIAgentSessionsModelEvent::Ended { .. } => {}
            },
        );
    }

    /// Handle events re-emitted by the `TerminalDriver`.
    fn handle_terminal_driver_event(
        &mut self,
        event: &TerminalDriverEvent,
        ctx: &mut ModelContext<Self>,
    ) {
        match event {
            TerminalDriverEvent::SlowBootstrap => {
                eprintln!(
                    "Warning: Terminal session is slow to bootstrap. See https://docs.warp.dev/support-and-community/troubleshooting-and-support/known-issues#shells to troubleshoot."
                );
            }
            TerminalDriverEvent::EstablishedSharedSession {
                session_id,
                join_url,
            } => {
                write_session_joined(join_url, self.output_format);

                // If running as part of a task, store the session-sharing link.
                if let Some(task_id) = self.task_id {
                    let server_api = ServerApiProvider::as_ref(ctx).get_ai_client();
                    let session_id = *session_id;
                    ctx.spawn(
                        async move {
                            report_if_error!(server_api
                                .update_agent_task(task_id, None, Some(session_id), None, None)
                                .await
                                .context("Error setting ambient agent shared session ID"));
                        },
                        |_, _, _| {},
                    );
                }
            }
        }
    }

    /// Perform cleanup after the agent has finished running. Previously walked
    /// per-CloudProvider cleanup hooks; the cloud-provider subsystem has been
    /// removed (Oz-only), leaving this as a no-op.
    async fn cleanup(_spawner: ModelSpawner<Self>) {}

    /// Invoke the end-of-run snapshot upload pipeline if the feature flag is enabled and this
    /// driver is associated with a cloud task. Errors are logged internally; this helper always
    /// returns so cleanup can proceed.
    async fn run_snapshot_upload(spawner: &ModelSpawner<Self>) {
        if !FeatureFlag::OzHandoff.is_enabled() {
            return;
        }

        // Snapshot upload is only meaningful for cloud task runs, so short-circuit before
        // pulling the rest of the context onto this task.
        let Ok((Some(task_id), snapshot_disabled, upload_timeout, script_timeout)) = spawner
            .spawn(|me, _| {
                (
                    me.task_id,
                    me.snapshot_disabled,
                    me.snapshot_upload_timeout,
                    me.snapshot_script_timeout,
                )
            })
            .await
        else {
            return;
        };
        if snapshot_disabled {
            log::info!("Skipping snapshot upload because --no-snapshot was specified");
            return;
        }

        let Ok((working_dir, client)) = spawner
            .spawn(|me, ctx| {
                let client = ServerApiProvider::as_ref(ctx).get_harness_support_client();
                (me.working_dir.clone(), client)
            })
            .await
        else {
            log::error!("Unable to retrieve snapshot upload context for cleanup (task {task_id})");
            return;
        };

        // Regenerate the declarations file so the upload pipeline sees the latest workspace
        // state. The helper swallows its own errors at ERROR level; we just proceed.
        snapshot::run_declarations_script(&working_dir, &task_id, script_timeout).await;

        // Cap the upload so a pathological slow upload cannot wedge cleanup.
        // On timeout we surface via report_error! so Sentry captures the incident and on-call
        // alerting can fire, then let cloud-provider teardown continue.
        if let Err(TimeoutError) = snapshot::upload_snapshot_from_declarations(client, &task_id)
            .with_timeout(upload_timeout)
            .await
        {
            report_error!(anyhow!(
                "Snapshot upload timed out after {:?}; continuing with cleanup (task {task_id})",
                upload_timeout
            ));
        }
    }
}

impl Entity for AgentDriver {
    type Event = ();
}

/// The only reason that `AgentDriver` is a singleton entity is to ensure the UI framework
/// doesn't drop it. Generally, we should not assume there's only one running agent.
impl SingletonEntity for AgentDriver {}

/// Write the run ID to stdout using the appropriate output format.
pub(super) fn write_run_started(run_id: &str, output_format: OutputFormat) {
    report_if_error!(output::with_stdout_buffered(|buf| match output_format {
        OutputFormat::Json | OutputFormat::Ndjson => output::json::run_started(run_id, buf),
        OutputFormat::Text | OutputFormat::Pretty => output::text::run_started(run_id, buf),
    })
    .context("Failed to write run ID"));
}

/// Report a driver-level error to the server for the given task.
///
/// Used for errors that occur before or outside a conversation. Errors
/// that occur while the agent is running should be reported through
/// the `TaskStatusSyncModel`.
pub(super) async fn report_driver_error(
    task_id: AmbientAgentTaskId,
    err: &AgentDriverError,
    server_api: &Arc<dyn AIClient>,
) {
    let (state, status_update) = error_classification::classify_driver_error(err);
    if let Err(e) = server_api
        .update_agent_task(task_id, Some(state), None, None, Some(status_update))
        .await
    {
        report_error!(
            anyhow!(e).context(format!("Failed to report driver error for task {task_id}"))
        );
    }
}

/// Write the session URL to stdout using the appropriate output format
fn write_session_joined(join_url: &str, output_format: OutputFormat) {
    report_if_error!(output::with_stdout_buffered(|buf| match output_format {
        OutputFormat::Json | OutputFormat::Ndjson =>
            output::json::shared_session_established(join_url, buf),
        OutputFormat::Text | OutputFormat::Pretty => {
            output::text::shared_session_established(join_url, buf)
        }
    })
    .context("Failed to write shared session event"));
}

#[cfg(test)]
#[path = "driver_tests.rs"]
mod tests;
