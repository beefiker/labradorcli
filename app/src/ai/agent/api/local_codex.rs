use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    env,
    path::{Path, PathBuf},
    process::{Command as StdCommand, Stdio},
    sync::OnceLock,
    time::Duration,
};

use async_io::Timer;

use ai::{local_claude_auth, local_openai_auth};
use command::r#async::Command;
use futures::Stream;
use futures_lite::{io::BufReader, stream, AsyncBufReadExt, StreamExt as _};
use prost_types::FieldMask;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;
use warp_core::channel::ChannelState;
use warp_multi_agent_api as api;

use crate::ai::agent::AIAgentInput;
use crate::ai::predict::generate_ai_input_suggestions::{
    GenerateAIInputSuggestionsRequest, GenerateAIInputSuggestionsResponseV2,
};
use crate::ai::predict::generate_am_query_suggestions::{
    GenerateAMQuerySuggestionsRequest, GenerateAMQuerySuggestionsResponse,
    SimpleQuery as AMQuerySimpleQuery, Suggestion as AMQuerySuggestion,
};

use super::{ConvertToAPITypeError, RequestParams, ResponseStream};

const CODEX_BIN_ENV_VAR: &str = "DWARF_CODEX_BIN";
const CODEX_MODEL_ENV_VAR: &str = "DWARF_CODEX_MODEL";
const CLAUDE_BIN_ENV_VAR: &str = "DWARF_CLAUDE_BIN";
const CLAUDE_MODEL_ENV_VAR: &str = "DWARF_CLAUDE_MODEL";
const LOCAL_AGENT_ENV_VAR: &str = "DWARF_LOCAL_AGENT";
const LOCAL_AGENT_DISPLAY_NAME: &str = "Local Agent";
const TOOL_CALL_PREFIX: &str = "DWARF_TOOL_CALL";
const CLAUDE_DEFAULT_MODEL_ID: &str = "claude-code";
const LOCAL_AGENT_TIMEOUT_ENV_VAR: &str = "DWARF_LOCAL_AGENT_TIMEOUT_SECS";
const DEFAULT_LOCAL_AGENT_TIMEOUT_SECS: u64 = 300;
const LOCAL_CLI_TOOL_OUTPUT_SERVER_DATA_TYPE: &str = "local_cli_tool_output";

const MODEL_IDENTITY_PROSE: &str = "If the user asks what model you are, report this configured model label and do not claim a separate runtime label you cannot inspect.\n\n";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LocalAgentRuntime {
    Codex,
    Claude,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LocalAuthState {
    codex: bool,
    claude: bool,
}

impl LocalAgentRuntime {
    fn for_standalone_request() -> Self {
        if let Some(runtime) = configured_local_agent_runtime() {
            return runtime;
        }

        if local_claude_auth::has_auth_state() && !local_openai_auth::has_access_token() {
            LocalAgentRuntime::Claude
        } else {
            LocalAgentRuntime::Codex
        }
    }

    fn slug(self) -> &'static str {
        match self {
            LocalAgentRuntime::Codex => "codex",
            LocalAgentRuntime::Claude => "claude",
        }
    }

    fn cli_name(self) -> &'static str {
        match self {
            LocalAgentRuntime::Codex => "Codex CLI",
            LocalAgentRuntime::Claude => "Claude Code CLI",
        }
    }
}

impl LocalAuthState {
    fn current() -> Self {
        Self {
            codex: local_openai_auth::has_access_token(),
            claude: local_claude_auth::has_auth_state(),
        }
    }

    fn has_runtime(self, runtime: LocalAgentRuntime) -> bool {
        match runtime {
            LocalAgentRuntime::Codex => self.codex,
            LocalAgentRuntime::Claude => self.claude,
        }
    }

    fn only_available_runtime(self) -> Option<LocalAgentRuntime> {
        match (self.codex, self.claude) {
            (true, false) => Some(LocalAgentRuntime::Codex),
            (false, true) => Some(LocalAgentRuntime::Claude),
            _ => None,
        }
    }
}

fn runtime_for_request(params: &RequestParams) -> LocalAgentRuntime {
    if let Some(runtime) = configured_local_agent_runtime() {
        return runtime;
    }

    runtime_for_model(params.model.as_str(), LocalAuthState::current())
}

fn runtime_for_model(model: &str, auth_state: LocalAuthState) -> LocalAgentRuntime {
    let requested_runtime = local_agent_runtime_for_model(model);
    if auth_state.has_runtime(requested_runtime) {
        return requested_runtime;
    }

    if is_defaultish_model_for_runtime(model, requested_runtime) {
        if let Some(runtime) = auth_state.only_available_runtime() {
            return runtime;
        }
    }

    requested_runtime
}

fn configured_local_agent_runtime() -> Option<LocalAgentRuntime> {
    let value = env::var(LOCAL_AGENT_ENV_VAR).ok()?;
    match value.trim().to_ascii_lowercase().as_str() {
        "codex" | "openai" => Some(LocalAgentRuntime::Codex),
        "claude" | "claude-code" | "claude_code" => Some(LocalAgentRuntime::Claude),
        _ => None,
    }
}

fn local_agent_runtime_for_model(model: &str) -> LocalAgentRuntime {
    if is_claude_request_model(model) {
        LocalAgentRuntime::Claude
    } else {
        LocalAgentRuntime::Codex
    }
}

fn is_defaultish_model_for_runtime(model: &str, runtime: LocalAgentRuntime) -> bool {
    let model = model.trim().to_ascii_lowercase();
    match runtime {
        LocalAgentRuntime::Codex => matches!(model.as_str(), "auto" | "gpt-5.5"),
        LocalAgentRuntime::Claude => is_claude_default_model(&model),
    }
}

pub(super) async fn generate_output(
    params: RequestParams,
    cancellation_rx: futures::channel::oneshot::Receiver<()>,
) -> Result<ResponseStream, ConvertToAPITypeError> {
    let runtime = runtime_for_request(&params);
    let request_id = format!("local-{}-request-{}", runtime.slug(), Uuid::new_v4());
    let conversation_id = params
        .conversation_token
        .as_ref()
        .map(|token| token.as_str().to_string())
        .unwrap_or_else(|| format!("local-{}-conversation-{}", runtime.slug(), Uuid::new_v4()));

    let root_task_id = root_task_id(&params.tasks);
    let needs_create_task = root_task_id.is_none();
    let task_id =
        root_task_id.unwrap_or_else(|| format!("local-{}-task-{}", runtime.slug(), Uuid::new_v4()));
    let working_directory = params
        .session_context
        .current_working_directory()
        .as_deref()
        .map(str::to_string);

    // Fast path: when we can synthesize the tool call from the prompt alone, skip the CLI roundtrip
    // and emit the whole exchange in one shot — there's nothing to stream, so cancellation_rx is
    // unused on this branch (drops at end of scope).
    if let Some(tool_call) = direct_terminal_tool_call(&params.input, working_directory.as_deref())
    {
        let app_name = ChannelState::app_name_display();
        let output_text = if tool_call.command.starts_with("target=$(mdfind ") {
            format!("I'll find the matching directory and switch {app_name} to it.")
        } else {
            format!("I'll run `{}` in {app_name}.", tool_call.command)
        };
        return Ok(Box::pin(stream::iter(non_streaming_events(
            conversation_id,
            request_id,
            task_id,
            needs_create_task,
            output_text,
            vec![tool_call],
            &params.input,
            runtime,
        ))));
    }

    // Streaming path: compute model + prompt, build the runtime-specific delta stream once,
    // then move everything (including the user input) into the response-event stream by value —
    // no `params.input.clone()` just to satisfy a borrow.
    let (model, prompt) = match runtime {
        LocalAgentRuntime::Codex => {
            let m = model_for_codex(&params);
            let p = prompt_for_local_agent(&params, runtime, m.as_deref());
            (m, p)
        }
        LocalAgentRuntime::Claude => {
            let m = model_for_claude(&params);
            let p = prompt_for_local_agent(&params, runtime, m.as_deref());
            (m, p)
        }
    };
    let delta_stream: std::pin::Pin<
        Box<dyn Stream<Item = Result<LocalAgentStreamDelta, String>> + Send>,
    > = match runtime {
        LocalAgentRuntime::Codex => Box::pin(run_codex_streaming(
            prompt,
            working_directory.clone(),
            model,
        )),
        LocalAgentRuntime::Claude => Box::pin(run_claude_streaming(
            prompt,
            working_directory.clone(),
            model,
        )),
    };
    let inputs = params.input;
    let stream = async_stream::stream! {
        let mut delta_stream = delta_stream;
        yield Ok(init_event(conversation_id, request_id.clone()));
        if needs_create_task {
            yield Ok(client_actions_event(vec![api::ClientAction {
                action: Some(api::client_action::Action::CreateTask(
                    api::client_action::CreateTask {
                        task: Some(api::Task {
                            id: task_id.clone(),
                            description: LOCAL_AGENT_DISPLAY_NAME.to_string(),
                            dependencies: None,
                            messages: vec![],
                            summary: String::new(),
                            server_data: String::new(),
                        }),
                    },
                )),
            }]));
        }

        let seed_messages = user_query_messages(&inputs, &task_id, &request_id);
        if !seed_messages.is_empty() {
            yield Ok(client_actions_event(vec![api::ClientAction {
                action: Some(api::client_action::Action::AddMessagesToTask(
                    api::client_action::AddMessagesToTask {
                        task_id: task_id.clone(),
                        messages: seed_messages,
                    },
                )),
            }]));
        }

        // Accumulated *cleaned* text — what the user sees, with markers already stripped.
        // Used by the post-stream cwd-override path.
        let mut full_text = String::new();
        let mut current_text_message_id: Option<String> = None;
        let mut stream_filter = StreamingToolCallFilter::default();
        let mut streamed_tool_calls: Vec<LocalRunShellCommand> = Vec::new();

        let mut cancellation_rx = cancellation_rx;
        let mut was_cancelled = false;
        let mut stream_error: Option<String> = None;
        loop {
            let next_fut = delta_stream.next();
            futures::pin_mut!(next_fut);
            match futures::future::select(next_fut, &mut cancellation_rx).await {
                futures::future::Either::Left((Some(Ok(delta)), _)) => {
                    if delta.is_empty() {
                        continue;
                    }
                    match delta {
                        LocalAgentStreamDelta::Text(chunk) => {
                            let filtered = stream_filter.ingest(&chunk);
                            for text in filtered.text_chunks {
                                full_text.push_str(&text);
                                yield Ok(add_or_append_agent_text_event(
                                    &task_id,
                                    &request_id,
                                    runtime,
                                    &mut current_text_message_id,
                                    text,
                                ));
                            }
                            for tc in filtered.tool_calls {
                                streamed_tool_calls.push(tc.clone());
                                yield Ok(client_actions_event(vec![api::ClientAction {
                                    action: Some(api::client_action::Action::AddMessagesToTask(
                                        api::client_action::AddMessagesToTask {
                                            task_id: task_id.clone(),
                                            messages: vec![run_shell_command_message(
                                                &task_id, &request_id, tc,
                                            )],
                                        },
                                    )),
                                }]));
                                current_text_message_id = None;
                            }
                        }
                        LocalAgentStreamDelta::LocalToolOutput(output) => {
                            yield Ok(local_tool_output_event(&task_id, &request_id, output));
                            current_text_message_id = None;
                        }
                    }
                }
                futures::future::Either::Left((Some(Err(error)), _)) => {
                    log::warn!("Local agent CLI error: {error}");
                    stream_error = Some(error);
                    break;
                }
                futures::future::Either::Left((None, _)) => break,
                futures::future::Either::Right(_) => {
                    was_cancelled = true;
                    // Drop the delta stream — its child was spawned with kill_on_drop, so the
                    // CLI subprocess is signalled now instead of running to natural completion.
                    drop(delta_stream);
                    break;
                }
            }
        }

        // Flush any trailing partial line — emits a final text delta or final tool call.
        let trailing = stream_filter.flush();
        for text in trailing.text_chunks {
            full_text.push_str(&text);
            yield Ok(add_or_append_agent_text_event(
                &task_id,
                &request_id,
                runtime,
                &mut current_text_message_id,
                text,
            ));
        }
        for tc in trailing.tool_calls {
            streamed_tool_calls.push(tc.clone());
            yield Ok(client_actions_event(vec![api::ClientAction {
                action: Some(api::client_action::Action::AddMessagesToTask(
                    api::client_action::AddMessagesToTask {
                        task_id: task_id.clone(),
                        messages: vec![run_shell_command_message(&task_id, &request_id, tc)],
                    },
                )),
            }]));
            current_text_message_id = None;
        }

        if let Some(error) = stream_error {
            let text = if current_text_message_id.is_some() && !full_text.trim().is_empty() {
                format!("\n\n{error}")
            } else {
                error
            };
            yield Ok(add_or_append_agent_text_event(
                &task_id,
                &request_id,
                runtime,
                &mut current_text_message_id,
                text,
            ));
            yield Ok(finished_event());
            return;
        }

        if was_cancelled {
            // Surface cancellation as a replace so the user sees the partial response was
            // intentionally stopped (instead of just ending mid-token). Append the suffix
            // in-place to avoid cloning the (possibly long) `full_text` into a new String.
            let text = if current_text_message_id.is_some() && !full_text.trim().is_empty() {
                "\n\n_Request cancelled._".to_string()
            } else {
                "_Request cancelled._".to_string()
            };
            yield Ok(add_or_append_agent_text_event(
                &task_id,
                &request_id,
                runtime,
                &mut current_text_message_id,
                text,
            ));
            yield Ok(finished_event());
            return;
        }

        // Post-process: cwd-change detection from the user's prompt + already-clean text.
        // Markers were extracted live during streaming; nothing to strip from text now.
        let mut cleaned_text = full_text.clone();
        let mut tool_calls: Vec<LocalRunShellCommand> = Vec::new();
        if let Some(target_dir) =
            local_cwd_change_target(&inputs, &cleaned_text, working_directory.as_deref())
        {
            let command = format!("cd {}", shell_quote_path(&target_dir));
            // Don't duplicate a cwd-change tool call that already streamed inline.
            let already_streamed = streamed_tool_calls.iter().any(|c| c.command == command);
            if !already_streamed {
                tool_calls.push(LocalRunShellCommand::read_only(command));
            }
            // When the synthesized cwd-change is the only action, simplify the prose
            // (matches behaviour of the previous post-process pass).
            if streamed_tool_calls.is_empty() && tool_calls.len() == 1 {
                cleaned_text = format!(
                    "I'll run `{}` in {}.",
                    tool_calls[0].command,
                    ChannelState::app_name_display()
                );
            }
        }
        log::info!(
            "[local-agent] stream summary: text_bytes={} streamed_tool_calls={} cwd_followup_tool_calls={}",
            full_text.len(),
            streamed_tool_calls.len(),
            tool_calls.len(),
        );

        if cleaned_text != full_text {
            if let Some(message_id) = &current_text_message_id {
                yield Ok(replace_agent_text_event(&task_id, message_id, cleaned_text));
            } else {
                yield Ok(add_or_append_agent_text_event(
                    &task_id,
                    &request_id,
                    runtime,
                    &mut current_text_message_id,
                    cleaned_text,
                ));
            }
        }

        if !tool_calls.is_empty() {
            let messages = tool_calls
                .into_iter()
                .map(|tool_call| run_shell_command_message(&task_id, &request_id, tool_call))
                .collect();
            yield Ok(client_actions_event(vec![api::ClientAction {
                action: Some(api::client_action::Action::AddMessagesToTask(
                    api::client_action::AddMessagesToTask { task_id, messages },
                )),
            }]));
        }

        yield Ok(finished_event());
    };

    Ok(Box::pin(stream))
}

fn non_streaming_events(
    conversation_id: String,
    request_id: String,
    task_id: String,
    needs_create_task: bool,
    output_text: String,
    tool_calls: Vec<LocalRunShellCommand>,
    inputs: &[AIAgentInput],
    runtime: LocalAgentRuntime,
) -> Vec<super::Event> {
    let mut events = vec![Ok(init_event(conversation_id, request_id.clone()))];
    if needs_create_task {
        events.push(Ok(client_actions_event(vec![api::ClientAction {
            action: Some(api::client_action::Action::CreateTask(
                api::client_action::CreateTask {
                    task: Some(api::Task {
                        id: task_id.clone(),
                        description: LOCAL_AGENT_DISPLAY_NAME.to_string(),
                        dependencies: None,
                        messages: vec![],
                        summary: String::new(),
                        server_data: String::new(),
                    }),
                },
            )),
        }])));
    }

    let mut messages = user_query_messages(inputs, &task_id, &request_id);
    messages.push(api::Message {
        id: format!("local-{}-message-{}", runtime.slug(), Uuid::new_v4()),
        task_id: task_id.clone(),
        request_id: request_id.clone(),
        timestamp: None,
        server_message_data: String::new(),
        citations: vec![],
        message: Some(api::message::Message::AgentOutput(
            api::message::AgentOutput { text: output_text },
        )),
    });
    for tool_call in tool_calls {
        messages.push(run_shell_command_message(&task_id, &request_id, tool_call));
    }
    events.push(Ok(client_actions_event(vec![api::ClientAction {
        action: Some(api::client_action::Action::AddMessagesToTask(
            api::client_action::AddMessagesToTask { task_id, messages },
        )),
    }])));
    events.push(Ok(finished_event()));
    events
}

pub(crate) async fn maybe_generate_local_ai_input_suggestions(
    request: &GenerateAIInputSuggestionsRequest,
) -> Option<Result<GenerateAIInputSuggestionsResponseV2, String>> {
    if !should_use_local_agent_for_suggestions() {
        return None;
    }

    Some(generate_local_ai_input_suggestions(request).await)
}

pub(crate) async fn maybe_generate_local_am_query_suggestions(
    request: &GenerateAMQuerySuggestionsRequest,
) -> Option<Result<GenerateAMQuerySuggestionsResponse, String>> {
    if !should_use_local_agent_for_suggestions() {
        return None;
    }

    Some(generate_local_am_query_suggestions(request).await)
}

async fn generate_local_ai_input_suggestions(
    request: &GenerateAIInputSuggestionsRequest,
) -> Result<GenerateAIInputSuggestionsResponseV2, String> {
    let prompt = prompt_for_local_ai_input_suggestion(request);
    let working_directory = working_directory_from_context_messages(&request.context_messages);
    let output = run_local_agent_for_suggestion(prompt, working_directory.as_deref()).await?;
    let command = extract_local_next_command(&output).unwrap_or_default();

    if command.is_empty() {
        return Ok(GenerateAIInputSuggestionsResponseV2::default());
    }

    Ok(GenerateAIInputSuggestionsResponseV2 {
        commands: vec![command.clone()],
        ai_queries: vec![],
        most_likely_action: command,
    })
}

async fn generate_local_am_query_suggestions(
    request: &GenerateAMQuerySuggestionsRequest,
) -> Result<GenerateAMQuerySuggestionsResponse, String> {
    let prompt = prompt_for_local_am_query_suggestion(request);
    let working_directory = working_directory_from_context_messages(&request.context_messages);
    let output = run_local_agent_for_suggestion(prompt, working_directory.as_deref()).await?;
    let query = extract_local_prompt_suggestion_query(&output).unwrap_or_default();

    Ok(GenerateAMQuerySuggestionsResponse {
        id: format!("local-suggestion-{}", Uuid::new_v4()),
        suggestion: (!query.is_empty()).then_some(AMQuerySuggestion::Simple(AMQuerySimpleQuery {
            query,
            should_plan_task: false,
        })),
    })
}

fn should_use_local_agent_for_suggestions() -> bool {
    local_openai_auth::has_access_token() || local_claude_auth::has_auth_state()
}

async fn run_local_agent_for_suggestion(
    prompt: String,
    working_directory: Option<&str>,
) -> Result<String, String> {
    match LocalAgentRuntime::for_standalone_request() {
        LocalAgentRuntime::Codex => {
            let model = standalone_codex_model();
            run_codex(prompt, working_directory, model.as_deref()).await
        }
        LocalAgentRuntime::Claude => {
            let model = standalone_claude_model();
            run_claude(prompt, working_directory, model.as_deref()).await
        }
    }
}

fn standalone_codex_model() -> Option<String> {
    selected_codex_model(env::var(CODEX_MODEL_ENV_VAR).ok(), "")
}

fn standalone_claude_model() -> Option<String> {
    selected_claude_model(env::var(CLAUDE_MODEL_ENV_VAR).ok(), CLAUDE_DEFAULT_MODEL_ID)
}

fn prompt_for_local_ai_input_suggestion(request: &GenerateAIInputSuggestionsRequest) -> String {
    let request_json = serde_json::to_string_pretty(request)
        .unwrap_or_else(|_| "Could not serialize suggestion context.".to_string());
    let app_name = ChannelState::app_name_display();
    format!(
        "You are generating a local {app_name} terminal autosuggestion.\n\
         Return exactly one JSON object and no prose: {{\"command\":\"...\"}}.\n\
         Suggest one safe shell command the user is likely to run next from the given terminal context.\n\
         Do not run anything. Do not include markdown. Do not include explanations.\n\
         If there is no useful next command, return {{\"command\":\"\"}}.\n\
         If a prefix exists, the command must start with that prefix.\n\n\
         Terminal suggestion context:\n{request_json}"
    )
}

fn prompt_for_local_am_query_suggestion(request: &GenerateAMQuerySuggestionsRequest) -> String {
    let request_json = serde_json::to_string_pretty(request)
        .unwrap_or_else(|_| "Could not serialize suggestion context.".to_string());
    let app_name = ChannelState::app_name_display();
    format!(
        "You are generating a local {app_name} agent follow-up chip.\n\
         Return exactly one JSON object and no prose: {{\"query\":\"...\"}}.\n\
         Suggest one concise natural-language instruction for the local {app_name} agent based on the terminal context.\n\
         Prefer instructions that ask {app_name} to inspect, run, test, debug, or summarize with local commands when useful.\n\
         Do not mention Warp, Oz, credits, cloud agents, accounts, or premium features.\n\
         Do not run anything. Do not include markdown. Do not include explanations.\n\
         If there is no useful follow-up, return {{\"query\":\"\"}}.\n\n\
         Agent suggestion context:\n{request_json}"
    )
}

#[derive(Debug, Deserialize)]
struct LocalNextCommandJson {
    #[serde(default)]
    command: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LocalPromptSuggestionJson {
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    prompt: Option<String>,
}

fn extract_local_next_command(output: &str) -> Option<String> {
    let response = parse_local_json_response::<LocalNextCommandJson>(output)?;
    response
        .command
        .map(normalize_one_line)
        .filter(|s| !s.is_empty())
}

fn extract_local_prompt_suggestion_query(output: &str) -> Option<String> {
    let response = parse_local_json_response::<LocalPromptSuggestionJson>(output)?;
    response
        .query
        .or(response.prompt)
        .map(normalize_one_line)
        .filter(|s| !s.is_empty())
}

fn parse_local_json_response<T: DeserializeOwned>(output: &str) -> Option<T> {
    let candidate = local_json_object_candidate(output)?;
    serde_json::from_str(candidate).ok()
}

fn local_json_object_candidate(output: &str) -> Option<&str> {
    let output = output.trim();
    if let Some(fenced) = fenced_json_body(output) {
        return Some(fenced);
    }

    let start = output.find('{')?;
    let end = output.rfind('}')?;
    (start <= end).then_some(&output[start..=end])
}

fn fenced_json_body(output: &str) -> Option<&str> {
    let body = output.strip_prefix("```")?;
    let body = body
        .strip_prefix("json")
        .or_else(|| body.strip_prefix("JSON"))
        .unwrap_or(body)
        .trim_start();
    let end = body.rfind("```")?;
    Some(body[..end].trim())
}

fn normalize_one_line(value: String) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn working_directory_from_context_messages(context_messages: &[String]) -> Option<String> {
    context_messages
        .iter()
        .rev()
        .filter_map(|message| serde_json::from_str::<Value>(message).ok())
        .filter_map(|value| {
            value
                .get("pwd")
                .or_else(|| value.pointer("/context/pwd"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|pwd| !pwd.is_empty())
                .map(str::to_string)
        })
        .find(|pwd| Path::new(pwd).is_dir())
}

fn run_codex_streaming(
    prompt: String,
    working_directory: Option<String>,
    model: Option<String>,
) -> impl Stream<Item = Result<LocalAgentStreamDelta, String>> + Send {
    async_stream::stream! {
        let codex_bin = codex_bin();
        let mut command = Command::new(codex_bin.clone());
        command
            .arg("exec")
            .arg("--json")
            .arg("--skip-git-repo-check")
            .arg("--sandbox")
            .arg("workspace-write");
        if let Some(model) = model.as_deref() {
            command.arg("--model").arg(model);
        }
        if let Some(wd) = working_directory.as_deref() {
            command.arg("-C").arg(wd);
        }
        command.arg(prompt);
        command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                yield Err(format!(
                    "{LOCAL_AGENT_DISPLAY_NAME} failed to start. Tried `{codex_bin}`. Set {CODEX_BIN_ENV_VAR} to your Codex CLI path if it is not on PATH.\n\n```text\n{error}\n```"
                ));
                return;
            }
        };

        let stdout = child.stdout.take().expect("stdout was piped");
        let stderr = child.stderr.take().expect("stderr was piped");
        let mut merged = merge_child_lines(stdout, stderr);

        let mut accumulator = CodexAccumulator::default();
        let mut stderr_buf = String::new();
        let timeout_total = local_agent_timeout();
        let mut timeout = Timer::after(timeout_total);

        loop {
            let next_fut = merged.next();
            futures::pin_mut!(next_fut);
            match futures::future::select(next_fut, &mut timeout).await {
                futures::future::Either::Left((Some(MergedLine::Stdout(line)), _)) => {
                    if let Some(delta) = accumulator.ingest_line(&line) {
                        if !delta.is_empty() {
                            yield Ok(delta);
                        }
                    }
                }
                futures::future::Either::Left((Some(MergedLine::Stderr(line)), _)) => {
                    append_stderr(&mut stderr_buf, &line);
                }
                futures::future::Either::Left((None, _)) => break,
                futures::future::Either::Right(_) => {
                    // Hard timeout — drop the child via merged (kill_on_drop fires).
                    drop(merged);
                    let _ = child.kill();
                    yield Err(format!(
                        "{LOCAL_AGENT_DISPLAY_NAME} Codex CLI timed out after {}s. Set {LOCAL_AGENT_TIMEOUT_ENV_VAR} to override.",
                        timeout_total.as_secs()
                    ));
                    return;
                }
            }
        }

        let status = match child.status().await {
            Ok(status) => status,
            Err(error) => {
                yield Err(format!(
                    "{LOCAL_AGENT_DISPLAY_NAME} failed waiting on Codex CLI.\n\n```text\n{error}\n```"
                ));
                return;
            }
        };

        if status.success() {
            if !accumulator.has_emitted_any {
                // Codex completed cleanly but never produced an `agent_message`. Try the
                // reasoning summaries (if any) as a fallback before failing — this is the
                // common case for prompts that drive Codex into agent/tool mode where it
                // thinks but never emits a user-facing message.
                if !accumulator.reasoning_summaries.is_empty() {
                    let summary = accumulator.reasoning_summaries.join("\n\n");
                    yield Ok(LocalAgentStreamDelta::Text(format!(
                        "_Codex finished without a final answer; surfacing reasoning summary instead._\n\n{summary}"
                    )));
                } else {
                    log::warn!(
                        "Codex finished without agent_message. Item types seen: {:?}. stderr: {}",
                        accumulator.non_message_item_counts,
                        truncate(&stderr_buf, 1_000)
                    );
                    yield Err(format!(
                        "{LOCAL_AGENT_DISPLAY_NAME} finished without an agent message. Item types seen: {:?}.\n\n```text\n{}\n```",
                        accumulator.non_message_item_counts,
                        truncate(&stderr_buf, 4_000)
                    ));
                }
            }
        } else {
            yield Err(format!(
                "{LOCAL_AGENT_DISPLAY_NAME} exited with status {}.\n\n```text\n{}\n```",
                status,
                truncate(&stderr_buf, 4_000)
            ));
        }
    }
}

async fn run_codex(
    prompt: String,
    working_directory: Option<&str>,
    model: Option<&str>,
) -> Result<String, String> {
    let mut stream = Box::pin(run_codex_streaming(
        prompt,
        working_directory.map(str::to_string),
        model.map(str::to_string),
    ));
    let mut full = String::new();
    while let Some(item) = stream.next().await {
        match item {
            Ok(LocalAgentStreamDelta::Text(chunk)) => full.push_str(&chunk),
            Ok(LocalAgentStreamDelta::LocalToolOutput(output)) => {
                append_local_tool_output_as_text(&mut full, &output);
            }
            Err(error) => return Err(error),
        }
    }
    Ok(full)
}

async fn run_claude(
    prompt: String,
    working_directory: Option<&str>,
    model: Option<&str>,
) -> Result<String, String> {
    let claude_bin = claude_bin();
    let mut command = Command::new(claude_bin.clone());
    command
        .arg("-p")
        .arg("--output-format")
        .arg("json")
        .arg("--tools")
        .arg("")
        .arg("--no-session-persistence");
    if let Some(model) = model {
        command.arg("--model").arg(model);
    }
    if let Some(working_directory) = working_directory {
        command.current_dir(working_directory);
    }
    command.arg(prompt);
    // Without kill_on_drop, dropping this future (caller cancellation, racing futures
    // in suggestion paths) leaves a zombie Claude Code process burning until it exits.
    command.kill_on_drop(true);

    let output = command.output().await.map_err(|error| {
        format!(
            "{LOCAL_AGENT_DISPLAY_NAME} failed to start Claude Code. Tried `{claude_bin}`. Set {CLAUDE_BIN_ENV_VAR} to your Claude Code CLI path if it is not on PATH.\n\n```text\n{error}\n```"
        )
    })?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if output.status.success() {
        extract_claude_agent_text(&stdout).ok_or_else(|| {
            format!(
                "{LOCAL_AGENT_DISPLAY_NAME} finished without a Claude Code message.\n\n```text\n{}\n```",
                truncate(&stdout, 4_000)
            )
        })
    } else {
        let details = if stderr.trim().is_empty() {
            stdout.as_ref()
        } else {
            stderr.as_ref()
        };
        Err(format!(
            "{LOCAL_AGENT_DISPLAY_NAME} Claude Code exited with status {}.\n\n```text\n{}\n```",
            output.status,
            truncate(details, 4_000)
        ))
    }
}

fn run_claude_streaming(
    prompt: String,
    working_directory: Option<String>,
    model: Option<String>,
) -> impl Stream<Item = Result<LocalAgentStreamDelta, String>> + Send {
    async_stream::stream! {
        let claude_bin = claude_bin();
        let mut command = Command::new(claude_bin.clone());
        command
            .arg("-p")
            .arg("--output-format")
            .arg("stream-json")
            .arg("--verbose")
            // Emit per-token `content_block_delta` chunks as they arrive instead of one
            // event per assistant turn. The parser below treats these as the source of
            // truth and suppresses the redundant whole-turn `assistant` event.
            .arg("--include-partial-messages")
            .arg("--no-session-persistence");
        if let Some(model) = model.as_deref() {
            command.arg("--model").arg(model);
        }
        if let Some(wd) = working_directory.as_deref() {
            command.current_dir(wd);
        }
        command.arg(prompt);
        command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                yield Err(format!(
                    "{LOCAL_AGENT_DISPLAY_NAME} failed to start Claude Code. Tried `{claude_bin}`. Set {CLAUDE_BIN_ENV_VAR} to your Claude Code CLI path if it is not on PATH.\n\n```text\n{error}\n```"
                ));
                return;
            }
        };

        let stdout = child.stdout.take().expect("stdout was piped");
        let stderr = child.stderr.take().expect("stderr was piped");
        let mut merged = merge_child_lines(stdout, stderr);

        let mut accumulator = ClaudeAccumulator::default();
        let mut stderr_buf = String::new();
        let timeout_total = local_agent_timeout();
        let mut timeout = Timer::after(timeout_total);

        loop {
            let next_fut = merged.next();
            futures::pin_mut!(next_fut);
            match futures::future::select(next_fut, &mut timeout).await {
                futures::future::Either::Left((Some(MergedLine::Stdout(line)), _)) => {
                    for delta in accumulator.ingest_line(&line) {
                        if !delta.is_empty() {
                            yield Ok(delta);
                        }
                    }
                }
                futures::future::Either::Left((Some(MergedLine::Stderr(line)), _)) => {
                    append_stderr(&mut stderr_buf, &line);
                }
                futures::future::Either::Left((None, _)) => break,
                futures::future::Either::Right(_) => {
                    drop(merged);
                    let _ = child.kill();
                    yield Err(format!(
                        "{LOCAL_AGENT_DISPLAY_NAME} Claude Code timed out after {}s. Set {LOCAL_AGENT_TIMEOUT_ENV_VAR} to override.",
                        timeout_total.as_secs()
                    ));
                    return;
                }
            }
        }

        let status = match child.status().await {
            Ok(status) => status,
            Err(error) => {
                yield Err(format!(
                    "{LOCAL_AGENT_DISPLAY_NAME} failed waiting on Claude Code.\n\n```text\n{error}\n```"
                ));
                return;
            }
        };

        if status.success() {
            if !accumulator.has_emitted_any {
                yield Err(format!(
                    "{LOCAL_AGENT_DISPLAY_NAME} finished without a Claude Code message.\n\n```text\n{}\n```",
                    truncate(&stderr_buf, 4_000)
                ));
            }
        } else {
            yield Err(format!(
                "{LOCAL_AGENT_DISPLAY_NAME} Claude Code exited with status {}.\n\n```text\n{}\n```",
                status,
                truncate(&stderr_buf, 4_000)
            ));
        }
    }
}

enum MergedLine {
    Stdout(String),
    Stderr(String),
}

/// Wraps the child's piped stdout/stderr in `BufReader`s and merges the two line
/// streams via `futures::stream::select`. Factored so the streaming helpers share
/// one setup path. Generic over the reader types to avoid pulling `async_process`
/// directly into this module.
fn merge_child_lines<R1, R2>(
    stdout: R1,
    stderr: R2,
) -> std::pin::Pin<Box<dyn Stream<Item = MergedLine> + Send>>
where
    R1: futures_lite::io::AsyncRead + Unpin + Send + 'static,
    R2: futures_lite::io::AsyncRead + Unpin + Send + 'static,
{
    let stdout_stream: std::pin::Pin<Box<dyn Stream<Item = MergedLine> + Send>> = Box::pin(
        BufReader::with_capacity(LINE_READER_BUF_BYTES, stdout)
            .lines()
            .filter_map(|r| r.ok().map(MergedLine::Stdout)),
    );
    let stderr_stream: std::pin::Pin<Box<dyn Stream<Item = MergedLine> + Send>> = Box::pin(
        BufReader::with_capacity(LINE_READER_BUF_BYTES, stderr)
            .lines()
            .filter_map(|r| r.ok().map(MergedLine::Stderr)),
    );
    Box::pin(futures::stream::select(stdout_stream, stderr_stream))
}

/// Stop growing stderr beyond this cap. The trailing error message is `truncate`d
/// to 4 000 chars anyway, so anything past ~8 KB is retained memory we never use.
const STDERR_CAP_BYTES: usize = 8 * 1024;

/// `BufReader` line buffer capacity. Both CLIs emit JSON lines that can exceed the
/// default 8 KB (Claude `--verbose` emits a ~3-5 KB `system.init` line full of MCP
/// tool descriptors; long Codex tool-call lines can exceed it too). Starting at 64 KB
/// avoids the per-oversize-line resize cost.
const LINE_READER_BUF_BYTES: usize = 64 * 1024;

fn local_agent_timeout() -> Duration {
    let secs = env::var(LOCAL_AGENT_TIMEOUT_ENV_VAR)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|secs| *secs > 0)
        .unwrap_or(DEFAULT_LOCAL_AGENT_TIMEOUT_SECS);
    Duration::from_secs(secs)
}

fn append_stderr(buf: &mut String, line: &str) {
    if buf.len() >= STDERR_CAP_BYTES {
        return;
    }
    if !buf.is_empty() {
        buf.push('\n');
    }
    buf.push_str(line);
}

/// Codex JSONL events we care about:
///   - `item.started` for `command_execution` / `web_search` — fires when Codex
///     kicks off a tool call. We narrate immediately so the user sees what
///     Codex is doing without waiting for the result.
///   - `item.completed` for `agent_message` (assistant text), `command_execution`
///     (with `aggregated_output` + `exit_code`), `web_search` (final `query`),
///     and `reasoning` (kept as fallback when no agent_message ever arrives).
///   - Older Codex versions emitted bare `{item: {...}}` without a top-level
///     `type` and grew text monotonically across item.started/updated/completed.
///     The accumulator still handles that shape for backward compat.
#[derive(Debug, Deserialize)]
struct CodexLine<'a> {
    /// Top-level event kind, e.g. `item.started` / `item.completed` /
    /// `turn.completed`. Empty when the event has no `type` field.
    #[serde(rename = "type", default, borrow)]
    kind: Cow<'a, str>,
    #[serde(default, borrow)]
    item: Option<CodexItem<'a>>,
}

/// Fields are `Cow<'a, str>` rather than `&'a str` because serde-json cannot
/// borrow a string slice when the JSON value contains escape sequences (a
/// runtime-decoded `String` is needed). Shell output, assistant text, and
/// reasoning summaries all routinely contain `\n` escapes; `Cow` borrows when
/// safe and allocates only when escapes force it.
#[derive(Debug, Deserialize)]
struct CodexItem<'a> {
    #[serde(rename = "type", default, borrow)]
    kind: Cow<'a, str>,
    #[serde(default, borrow)]
    id: Cow<'a, str>,
    #[serde(default, borrow)]
    text: Cow<'a, str>,
    /// Codex emits reasoning items as `{type: "reasoning", summary: "..."}`. We capture
    /// `summary` so a Codex run that "thinks" but never produces an agent_message can
    /// still surface its conclusion to the user instead of failing silently.
    #[serde(default, borrow)]
    summary: Cow<'a, str>,
    /// For `command_execution` items: the shell command Codex is running in its
    /// own sandbox (typically prefixed with `/bin/zsh -lc` or `/bin/bash -lc`).
    #[serde(default, borrow)]
    command: Cow<'a, str>,
    /// For completed `command_execution` items: the shell's combined stdout/stderr.
    #[serde(default, borrow)]
    aggregated_output: Cow<'a, str>,
    /// For completed `command_execution` items: the shell exit code. `None` while
    /// the command is still running.
    #[serde(default)]
    exit_code: Option<i32>,
    /// For `web_search` items: on completion, the final search query Codex issued.
    #[serde(default, borrow)]
    query: Cow<'a, str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum LocalAgentStreamDelta {
    Text(String),
    LocalToolOutput(LocalToolOutputDelta),
}

impl LocalAgentStreamDelta {
    fn is_empty(&self) -> bool {
        match self {
            LocalAgentStreamDelta::Text(text) => text.is_empty(),
            LocalAgentStreamDelta::LocalToolOutput(output) => {
                output.title.is_empty() && output.body.is_empty()
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LocalToolOutputDelta {
    item_id: String,
    title: String,
    body: String,
    is_complete: bool,
    is_error: bool,
    is_update: bool,
}

#[derive(Serialize)]
struct LocalCLIToolOutputServerData<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    title: &'a str,
    is_complete: bool,
    is_error: bool,
}

/// Tracks Codex stream state. Holds:
///   - per-id assistant text (for the older monotonic-update shape — current
///     Codex 0.130+ only emits item.completed for agent_message, but the
///     continuation logic is harmless and keeps backward compat),
///   - the set of in-flight tool call ids we've already narrated, so we don't
///     double-narrate if `item.started` fires twice or Codex retracts a call,
///   - sideband reasoning summaries + item-type counts for diagnostics.
#[derive(Default)]
struct CodexAccumulator {
    texts_by_id: HashMap<String, String>,
    last_message_id: Option<String>,
    has_emitted_any: bool,
    has_emitted_text: bool,
    /// Tool-call ids whose `item.started` narration we've already emitted. Lets
    /// us suppress the duplicate if `item.started` fires multiple times for the
    /// same id (defensive — Codex shouldn't, but we don't want to risk it).
    started_tool_ids: HashSet<String>,
    /// Reasoning summaries collected from `{type: "reasoning"}` items. Surfaced as
    /// a fallback response if no `agent_message` ever arrives.
    reasoning_summaries: Vec<String>,
    /// Count of item types we observed but didn't emit. Used to diagnose silent
    /// "no agent message" failures from the log.
    non_message_item_counts: HashMap<String, usize>,
}

impl CodexAccumulator {
    fn ingest_line(&mut self, line: &str) -> Option<LocalAgentStreamDelta> {
        let parsed: CodexLine = serde_json::from_str(line).ok()?;
        let item = parsed.item?;
        // `item.started` for `command_execution` is the right moment to narrate
        // a tool kickoff. Everything else routes through the completion handler,
        // which covers (a) the older monotonic agent_message growth across
        // item.started / item.updated / item.completed and (b) per-kind result
        // narration for command_execution and web_search.
        let event_kind: &str = &parsed.kind;
        let item_kind: &str = &item.kind;
        if event_kind == "item.started" && item_kind == "command_execution" {
            return self.handle_item_started(item);
        }
        self.handle_item_completed(item)
    }

    fn handle_item_started(&mut self, item: CodexItem) -> Option<LocalAgentStreamDelta> {
        let kind: &str = &item.kind;
        match kind {
            "command_execution" if !item.command.is_empty() => {
                if !self.started_tool_ids.insert(item.id.to_string()) {
                    return None;
                }
                let cmd = strip_shell_wrapper(&item.command);
                self.emit_tool_output(LocalToolOutputDelta {
                    item_id: item.id.into_owned(),
                    title: format!("Running {}", one_line(&cmd)),
                    body: String::new(),
                    is_complete: false,
                    is_error: false,
                    is_update: false,
                })
            }
            // Codex emits `item.started` for `web_search` with an empty query
            // (the query lands on `item.completed`), so there's nothing useful
            // to show yet — wait for the completion.
            _ => None,
        }
    }

    fn handle_item_completed(&mut self, item: CodexItem) -> Option<LocalAgentStreamDelta> {
        let kind: &str = &item.kind;
        match kind {
            "agent_message" if !item.id.is_empty() => self.handle_agent_message(item),
            "command_execution" => self.handle_command_completed(item),
            "web_search" if !item.query.is_empty() => {
                let q = one_line(&item.query);
                self.emit_tool_output(LocalToolOutputDelta {
                    item_id: item.id.into_owned(),
                    title: format!("Searched web for {q}"),
                    body: String::new(),
                    is_complete: true,
                    is_error: false,
                    is_update: false,
                })
            }
            "reasoning" => {
                if !item.summary.is_empty() {
                    self.reasoning_summaries.push(item.summary.into_owned());
                }
                *self
                    .non_message_item_counts
                    .entry("reasoning".to_string())
                    .or_default() += 1;
                None
            }
            kind => {
                if !kind.is_empty() {
                    *self
                        .non_message_item_counts
                        .entry(kind.to_string())
                        .or_default() += 1;
                }
                None
            }
        }
    }

    fn handle_command_completed(&mut self, item: CodexItem) -> Option<LocalAgentStreamDelta> {
        let output = item.aggregated_output.trim_end();
        let id_str: &str = &item.id;
        let is_update = self.started_tool_ids.contains(id_str);
        let cmd = strip_shell_wrapper(&item.command);
        let exit = item.exit_code.unwrap_or(0);
        if cmd.is_empty() && output.is_empty() && exit == 0 {
            return None;
        }

        let title = if cmd.is_empty() {
            "Ran command".to_string()
        } else {
            format!("Ran {}", one_line(&cmd))
        };
        self.emit_tool_output(LocalToolOutputDelta {
            item_id: item.id.into_owned(),
            title,
            body: format_tool_output_body(output, exit),
            is_complete: true,
            is_error: exit != 0,
            is_update,
        })
    }

    /// Handle assistant text. Codex 0.130 only emits item.completed for
    /// agent_message, but older streams emit growing item.started → updated →
    /// completed; the continuation logic handles both safely.
    fn handle_agent_message(&mut self, item: CodexItem) -> Option<LocalAgentStreamDelta> {
        let id: &str = &item.id;
        let new_text: &str = &item.text;

        // Continuation of the most recently seen message: emit the suffix only.
        let is_continuation = self.last_message_id.as_deref() == Some(id);
        if is_continuation {
            let prev = self
                .texts_by_id
                .get(id)
                .map(String::as_str)
                .unwrap_or_default();
            if new_text == prev {
                return None;
            }
            if new_text.starts_with(prev) {
                let delta = new_text[prev.len()..].to_string();
                self.texts_by_id
                    .insert(id.to_string(), new_text.to_string());
                self.has_emitted_any = true;
                self.has_emitted_text = true;
                return Some(LocalAgentStreamDelta::Text(delta));
            }
            // Non-monotonic update (text replaced rather than extended) — fall
            // through and emit the new text as a fresh paragraph.
        }

        let already_seen = self.texts_by_id.contains_key(id);
        self.texts_by_id
            .insert(id.to_string(), new_text.to_string());
        self.last_message_id = Some(id.to_string());

        if already_seen && !is_continuation {
            return None;
        }

        let sep = if self.has_emitted_text { "\n\n" } else { "" };
        let delta = format!("{sep}{new_text}");
        if delta.is_empty() {
            return None;
        }
        self.has_emitted_any = true;
        self.has_emitted_text = true;
        Some(LocalAgentStreamDelta::Text(delta))
    }

    fn emit_tool_output(&mut self, output: LocalToolOutputDelta) -> Option<LocalAgentStreamDelta> {
        self.has_emitted_any = true;
        self.has_emitted_text = false;
        self.last_message_id = None;
        Some(LocalAgentStreamDelta::LocalToolOutput(output))
    }
}

/// Strip the `/bin/zsh -lc` / `/bin/bash -lc` / `/bin/sh -c` wrapper Codex adds
/// to every shell command in its sandbox. Leaves the user-meaningful command
/// intact for display. Also handles the case where the inner command is wrapped
/// in matching quotes — those are dropped so the display matches what the user
/// would type at a prompt.
fn strip_shell_wrapper(command: &str) -> String {
    let trimmed = command.trim();
    for prefix in [
        "/bin/zsh -lc ",
        "/bin/bash -lc ",
        "/bin/sh -c ",
        "zsh -lc ",
        "bash -lc ",
        "sh -c ",
    ] {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            return unwrap_outer_quotes(rest.trim()).to_string();
        }
    }
    trimmed.to_string()
}

fn unwrap_outer_quotes(value: &str) -> &str {
    if value.len() >= 2 {
        let first = value.chars().next().unwrap();
        let last = value.chars().last().unwrap();
        if (first == '"' && last == '"') || (first == '\'' && last == '\'') {
            return &value[1..value.len() - 1];
        }
    }
    value
}

/// Claude `--output-format stream-json --include-partial-messages` events we
/// consume:
///   - `stream_event` carrying the typed `event` object below — primary source
///     for per-token text deltas, tool_use input JSON deltas, and block lifecycle.
///   - `assistant`: whole-turn finalization (used only when no `stream_event`
///     deltas arrived for this turn, e.g. older Claude Code versions).
///   - `user`: subsequent turn whose `content[]` may contain `tool_result` blocks
///     paired to a prior `tool_use_id`. We render these as structured local CLI
///     output messages so arbitrary tool output cannot corrupt markdown parsing.
///   - `result`: last-resort fallback if nothing else ever produced text.
#[derive(Debug, Deserialize)]
struct ClaudeLine<'a> {
    #[serde(rename = "type", default, borrow)]
    kind: &'a str,
    #[serde(default)]
    message: Option<ClaudeMessage>,
    #[serde(default, borrow)]
    result: Option<&'a str>,
    /// Nested event for `stream_event` items.
    #[serde(default)]
    event: Option<ClaudeStreamEvent>,
}

#[derive(Debug, Deserialize)]
struct ClaudeMessage {
    #[serde(default)]
    content: Vec<ClaudeContent>,
}

/// Content block as it appears in whole-turn `assistant` / `user` events. Tagged
/// by `type`; we care about text (assistant) and tool_result (user).
#[derive(Debug, Deserialize)]
struct ClaudeContent {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    text: Option<String>,
    /// For `tool_result` blocks. Claude Code emits this as either a plain string
    /// or an array of `{type: "text", text: ...}` parts; we accept both via
    /// `Value` and normalize to a string.
    #[serde(default)]
    content: Option<Value>,
    #[serde(default)]
    tool_use_id: Option<String>,
    #[serde(default)]
    is_error: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct ClaudeStreamEvent {
    #[serde(rename = "type", default)]
    kind: String,
    /// Block index for `content_block_*` events. Different blocks may interleave
    /// in principle; in practice Claude streams them sequentially, but we still
    /// index by `index` so out-of-order or parallel deltas don't cross-contaminate.
    #[serde(default)]
    index: Option<usize>,
    /// For `content_block_start` — the block we're starting.
    #[serde(default)]
    content_block: Option<ClaudeContentBlockStart>,
    /// For `content_block_delta` — the incremental update.
    #[serde(default)]
    delta: Option<ClaudeStreamDelta>,
}

#[derive(Debug, Deserialize)]
struct ClaudeContentBlockStart {
    #[serde(rename = "type", default)]
    kind: String,
    /// Present on `tool_use` blocks: the tool name (e.g. "Bash", "Read", "TodoWrite").
    #[serde(default)]
    name: Option<String>,
    /// Tool call id (e.g. "toolu_..."). Used to pair `tool_result` events to their
    /// originating `tool_use`.
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    input: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct ClaudeStreamDelta {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    text: Option<String>,
    /// For `input_json_delta` events: a fragment of the tool input JSON. We
    /// concatenate these in order and parse once at `content_block_stop`.
    #[serde(default)]
    partial_json: Option<String>,
}

/// Per-stream state for one Claude content block. We need to remember the block
/// kind + (for tool_use) the tool name + accumulated input JSON so we can render
/// a friendly inline summary when the block closes.
#[derive(Debug, Default)]
struct ClaudeContentBlockState {
    kind: String,
    id: Option<String>,
    tool_name: Option<String>,
    /// Accumulated `partial_json` fragments for `tool_use` blocks.
    tool_input_json: String,
}

#[derive(Debug)]
struct ClaudeToolUseDisplay {
    start_title: String,
    complete_title: String,
    body: String,
    completes_on_stop: bool,
}

#[derive(Debug)]
struct ClaudeToolUseRecord {
    complete_title: String,
}

/// Tracks Claude streaming state. With `--include-partial-messages` we receive
/// per-token text deltas, per-block tool_use lifecycle (start → input_json_delta+
/// → stop), and (in the next user-role event) any `tool_result` content. The
/// whole-turn `assistant` event arrives at the end with the same text
/// concatenated; we ignore it to avoid double-emitting.
#[derive(Default)]
struct ClaudeAccumulator {
    has_emitted_any: bool,
    has_emitted_text: bool,
    /// True once we've seen at least one `content_block_delta` token-level event.
    /// When true, suppress the subsequent whole-turn `assistant` event for the
    /// same content block — it would duplicate text we already emitted.
    has_streamed_partials: bool,
    /// Per-block-index state (text/thinking/tool_use). Populated on
    /// `content_block_start` and drained on `content_block_stop`.
    blocks: HashMap<usize, ClaudeContentBlockState>,
    /// Tool-call metadata keyed by Claude's `tool_use_id`, used to update the
    /// same local CLI output message when the matching `tool_result` arrives.
    tool_uses_by_id: HashMap<String, ClaudeToolUseRecord>,
}

impl ClaudeAccumulator {
    /// Ingest one JSONL line. Returns display deltas to forward to the user, if
    /// any. Assistant text remains text; Claude tool usage/results are emitted
    /// as structured local CLI output so raw command output is never interpreted
    /// as assistant markdown.
    fn ingest_line(&mut self, line: &str) -> Vec<LocalAgentStreamDelta> {
        let Ok(parsed) = serde_json::from_str::<ClaudeLine>(line) else {
            return vec![];
        };
        match parsed.kind {
            "stream_event" => parsed
                .event
                .and_then(|event| self.handle_stream_event(event))
                .into_iter()
                .collect(),
            // Older Claude Code CLI versions emit only whole-turn `assistant`
            // events. Use them only if we never saw a content_block_delta.
            "assistant" if !self.has_streamed_partials => {
                let Some(message) = parsed.message else {
                    return vec![];
                };
                let text: String = message
                    .content
                    .into_iter()
                    .filter_map(|c| (c.kind == "text").then_some(c.text).flatten())
                    .collect::<Vec<_>>()
                    .join("");
                self.emit_text(text).into_iter().collect()
            }
            // User-role events carry `tool_result` content for prior tool_use blocks.
            // Render each tool_result through the local CLI output UI instead of markdown.
            "user" => {
                let Some(message) = parsed.message else {
                    return vec![];
                };
                let mut out = Vec::new();
                for (idx, c) in message.content.into_iter().enumerate() {
                    if c.kind != "tool_result" {
                        continue;
                    }
                    let item_id = c
                        .tool_use_id
                        .clone()
                        .filter(|id| !id.is_empty())
                        .unwrap_or_else(|| format!("claude-tool-result-{idx}"));
                    let result_text = tool_result_to_text(&c);
                    let has_result_body = !result_text.trim().is_empty();
                    let Some(record) = self.tool_uses_by_id.get(&item_id) else {
                        if !has_result_body {
                            continue;
                        }
                        let is_error = c.is_error.unwrap_or(false);
                        self.has_emitted_any = true;
                        self.has_emitted_text = false;
                        out.push(LocalAgentStreamDelta::LocalToolOutput(
                            LocalToolOutputDelta {
                                item_id,
                                title: "Used tool".to_string(),
                                body: format_claude_tool_result_body(&result_text),
                                is_complete: true,
                                is_error,
                                is_update: false,
                            },
                        ));
                        continue;
                    };
                    let is_error = c.is_error.unwrap_or(false);
                    self.has_emitted_any = true;
                    self.has_emitted_text = false;
                    out.push(LocalAgentStreamDelta::LocalToolOutput(
                        LocalToolOutputDelta {
                            item_id,
                            title: record.complete_title.clone(),
                            body: format_claude_tool_result_body(&result_text),
                            is_complete: true,
                            is_error,
                            is_update: true,
                        },
                    ));
                }
                out
            }
            // Final fallback when neither stream_event nor assistant text fired.
            "result" if !self.has_emitted_any => {
                let Some(text) = parsed.result else {
                    return vec![];
                };
                self.emit_text(text.to_string()).into_iter().collect()
            }
            _ => vec![],
        }
    }

    fn handle_stream_event(&mut self, event: ClaudeStreamEvent) -> Option<LocalAgentStreamDelta> {
        match event.kind.as_str() {
            "content_block_start" => {
                let idx = event.index?;
                let block = event.content_block?;
                let input_json = block
                    .input
                    .as_ref()
                    .filter(|input| !input.is_null())
                    .and_then(|input| {
                        if input.as_object().is_some_and(serde_json::Map::is_empty) {
                            None
                        } else {
                            Some(input.to_string())
                        }
                    })
                    .unwrap_or_default();
                self.blocks.insert(
                    idx,
                    ClaudeContentBlockState {
                        kind: block.kind,
                        id: block.id,
                        tool_name: block.name,
                        tool_input_json: input_json,
                    },
                );
                None
            }
            "content_block_delta" => {
                let idx = event.index?;
                let delta = event.delta?;
                let block_kind = self
                    .blocks
                    .get(&idx)
                    .map(|b| b.kind.clone())
                    .unwrap_or_default();
                match delta.kind.as_str() {
                    "text_delta" => {
                        self.has_streamed_partials = true;
                        // Thinking blocks shouldn't surface as if they were assistant text;
                        // we render them in italic when the block closes (see _stop).
                        if block_kind == "text" {
                            self.emit_text(delta.text?)
                        } else {
                            None
                        }
                    }
                    "input_json_delta" => {
                        if let Some(state) = self.blocks.get_mut(&idx) {
                            if let Some(fragment) = delta.partial_json {
                                state.tool_input_json.push_str(&fragment);
                            }
                        }
                        None
                    }
                    // thinking_delta, signature_delta, citations_delta — collected but
                    // not surfaced as text.
                    _ => None,
                }
            }
            "content_block_stop" => {
                let idx = event.index?;
                let state = self.blocks.remove(&idx)?;
                if state.kind != "tool_use" {
                    return None;
                }
                let tool_name = state.tool_name.as_deref().unwrap_or("");
                let display = describe_claude_tool_use(tool_name, &state.tool_input_json);
                let item_id = state.id.unwrap_or_else(|| format!("claude-tool-{idx}"));
                self.tool_uses_by_id.insert(
                    item_id.clone(),
                    ClaudeToolUseRecord {
                        complete_title: display.complete_title.clone(),
                    },
                );
                self.has_emitted_any = true;
                self.has_emitted_text = false;
                Some(LocalAgentStreamDelta::LocalToolOutput(
                    LocalToolOutputDelta {
                        item_id,
                        title: if display.completes_on_stop {
                            display.complete_title
                        } else {
                            display.start_title
                        },
                        body: if display.completes_on_stop {
                            display.body
                        } else {
                            String::new()
                        },
                        is_complete: display.completes_on_stop,
                        is_error: false,
                        is_update: false,
                    },
                ))
            }
            // message_start / message_delta / message_stop — no payload we render.
            _ => None,
        }
    }

    /// Emit a primary text delta (assistant-authored text, not narration).
    fn emit_text(&mut self, text: String) -> Option<LocalAgentStreamDelta> {
        if text.is_empty() {
            return None;
        }
        let sep = if !self.has_emitted_text || self.has_streamed_partials {
            ""
        } else {
            "\n\n"
        };
        self.has_emitted_any = true;
        self.has_emitted_text = true;
        Some(LocalAgentStreamDelta::Text(format!("{sep}{text}")))
    }
}

/// Render a Claude `tool_result` block's `content` field to a plain string. The
/// field is either a bare string or an array of `{type: "text", text: ...}`
/// (and, less commonly, other block kinds we don't render).
fn tool_result_to_text(content: &ClaudeContent) -> String {
    let Some(value) = &content.content else {
        return String::new();
    };
    match value {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| {
                let kind = p.get("type").and_then(Value::as_str).unwrap_or("text");
                if kind != "text" {
                    return None;
                }
                p.get("text").and_then(Value::as_str).map(str::to_string)
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => value.to_string(),
    }
}

fn format_claude_tool_result_body(result_text: &str) -> String {
    let trimmed = result_text.trim_end();
    if trimmed.is_empty() {
        String::new()
    } else {
        truncate_tool_output_text(trimmed)
    }
}

fn format_tool_output_body(result_text: &str, exit_code: i32) -> String {
    let trimmed = result_text.trim_end();
    let mut body = if trimmed.is_empty() {
        String::new()
    } else {
        truncate_tool_output_text(trimmed)
    };
    if exit_code != 0 {
        if !body.is_empty() {
            body.push('\n');
        }
        body.push_str(&format!("(exit code {exit_code})"));
    }
    body
}

fn truncate_tool_output_text(result_text: &str) -> String {
    const MAX_LINES: usize = 12;
    const MAX_CHARS: usize = 1_200;
    let truncated_by_chars = result_text.len() > MAX_CHARS;
    let head: String = if truncated_by_chars {
        result_text.chars().take(MAX_CHARS).collect()
    } else {
        result_text.to_string()
    };
    let lines: Vec<&str> = head.lines().take(MAX_LINES).collect();
    let truncated_by_lines = head.lines().count() > MAX_LINES;
    let mut body = lines.join("\n");
    if truncated_by_lines || truncated_by_chars {
        body.push_str("\n...");
    }
    body
}

fn append_local_tool_output_as_text(text: &mut String, output: &LocalToolOutputDelta) {
    if text.is_empty() {
        text.push_str(&output.title);
    } else {
        text.push_str("\n\n");
        text.push_str(&output.title);
    }
    if !output.body.is_empty() {
        text.push('\n');
        text.push_str(&output.body);
    }
}

fn describe_claude_tool_use(tool_name: &str, input_json: &str) -> ClaudeToolUseDisplay {
    let parsed: Value = serde_json::from_str(input_json).unwrap_or(Value::Null);
    match tool_name {
        "Bash" => {
            let cmd = parsed
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();
            if cmd.is_empty() {
                claude_tool_display("Running command", "Ran command")
            } else {
                let cmd = one_line(cmd);
                claude_tool_display(&format!("Running {cmd}"), &format!("Ran {cmd}"))
            }
        }
        "Read" => describe_path_tool(&parsed, "Reading file", "Read file", "Reading", "Read"),
        "Edit" | "MultiEdit" => {
            describe_path_tool(&parsed, "Editing file", "Edited file", "Editing", "Edited")
        }
        "Write" => describe_path_tool(&parsed, "Writing file", "Wrote file", "Writing", "Wrote"),
        "Glob" => describe_pattern_tool(
            &parsed,
            "Searching files",
            "Searched files",
            "Searching",
            "Searched",
        ),
        "Grep" => describe_pattern_tool(
            &parsed,
            "Grepping",
            "Grepped",
            "Grepping for",
            "Grepped for",
        ),
        "WebFetch" => parsed
            .get("url")
            .and_then(Value::as_str)
            .map(|u| {
                let url = one_line(u);
                claude_tool_display(&format!("Fetching {url}"), &format!("Fetched {url}"))
            })
            .unwrap_or_else(|| claude_tool_display("Fetching URL", "Fetched URL")),
        "WebSearch" => parsed
            .get("query")
            .and_then(Value::as_str)
            .map(|q| {
                let query = one_line(q);
                claude_tool_display(
                    &format!("Searching web for {query}"),
                    &format!("Searched web for {query}"),
                )
            })
            .unwrap_or_else(|| claude_tool_display("Searching web", "Searched web")),
        "TodoWrite" => ClaudeToolUseDisplay {
            start_title: "Updating todos".to_string(),
            complete_title: "Updated todos".to_string(),
            body: format_todos_body(&parsed),
            completes_on_stop: true,
        },
        "" => claude_tool_display("Using tool", "Used tool"),
        other => claude_tool_display(&format!("Using {other}"), &format!("Used {other}")),
    }
}

fn claude_tool_display(start_title: &str, complete_title: &str) -> ClaudeToolUseDisplay {
    ClaudeToolUseDisplay {
        start_title: start_title.to_string(),
        complete_title: complete_title.to_string(),
        body: String::new(),
        completes_on_stop: false,
    }
}

fn describe_path_tool(
    parsed: &Value,
    default_start: &str,
    default_complete: &str,
    start_verb: &str,
    complete_verb: &str,
) -> ClaudeToolUseDisplay {
    let path = parsed
        .get("file_path")
        .or_else(|| parsed.get("path"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if path.is_empty() {
        claude_tool_display(default_start, default_complete)
    } else {
        let path = one_line(path);
        claude_tool_display(
            &format!("{start_verb} {path}"),
            &format!("{complete_verb} {path}"),
        )
    }
}

fn describe_pattern_tool(
    parsed: &Value,
    default_start: &str,
    default_complete: &str,
    start_verb: &str,
    complete_verb: &str,
) -> ClaudeToolUseDisplay {
    let pattern = parsed
        .get("pattern")
        .or_else(|| parsed.get("query"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if pattern.is_empty() {
        claude_tool_display(default_start, default_complete)
    } else {
        let pattern = one_line(pattern);
        claude_tool_display(
            &format!("{start_verb} {pattern}"),
            &format!("{complete_verb} {pattern}"),
        )
    }
}

fn format_todos_body(parsed: &Value) -> String {
    let Some(todos) = parsed.get("todos").and_then(Value::as_array) else {
        return String::new();
    };
    if todos.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    for todo in todos {
        let content = todo.get("content").and_then(Value::as_str).unwrap_or("");
        let status = todo.get("status").and_then(Value::as_str).unwrap_or("");
        let marker = match status {
            "completed" => "[x]",
            "in_progress" => "[~]",
            _ => "[ ]",
        };
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str("- ");
        out.push_str(marker);
        out.push(' ');
        out.push_str(content);
    }
    out
}

/// Collapse whitespace and trim. Keeps long file paths or commands on a single
/// line so the inline markdown stays compact.
fn one_line(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn codex_bin() -> String {
    if let Some(configured) = configured_codex_bin() {
        // Env-var override is intentionally re-read every call so users can rebind
        // without restart. Cheap (just an env lookup), unlike the shell resolver.
        return configured;
    }
    static CACHED: OnceLock<String> = OnceLock::new();
    CACHED
        .get_or_init(|| find_codex_bin().unwrap_or_else(|| "codex".to_string()))
        .clone()
}

fn claude_bin() -> String {
    if let Some(configured) = configured_claude_bin() {
        return configured;
    }
    static CACHED: OnceLock<String> = OnceLock::new();
    CACHED
        .get_or_init(|| find_claude_bin().unwrap_or_else(|| "claude".to_string()))
        .clone()
}

fn configured_codex_bin() -> Option<String> {
    env::var(CODEX_BIN_ENV_VAR)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn configured_claude_bin() -> Option<String> {
    env::var(CLAUDE_BIN_ENV_VAR)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn find_codex_bin() -> Option<String> {
    find_executable_in_path("codex")
        .or_else(find_common_codex_bin)
        .or_else(find_codex_with_user_shell)
}

fn find_claude_bin() -> Option<String> {
    find_executable_in_path("claude")
        .or_else(find_common_claude_bin)
        .or_else(find_claude_with_user_shell)
}

fn find_executable_in_path(name: &str) -> Option<String> {
    let paths = env::var_os("PATH")?;
    env::split_paths(&paths)
        .map(|path| path.join(name))
        .find(|path| is_executable(path))
        .map(path_to_string)
}

fn find_common_codex_bin() -> Option<String> {
    find_common_bin("codex")
}

fn find_common_claude_bin() -> Option<String> {
    find_common_bin("claude")
}

fn find_common_bin(binary_name: &str) -> Option<String> {
    common_candidates_for(binary_name)
        .into_iter()
        .find(|path| is_executable(path))
        .map(path_to_string)
}

fn common_candidates_for(binary_name: &str) -> Vec<PathBuf> {
    let mut candidates = vec![
        PathBuf::from(format!("/opt/homebrew/bin/{binary_name}")),
        PathBuf::from(format!("/usr/local/bin/{binary_name}")),
    ];

    if let Some(home) = env::var_os("HOME").map(PathBuf::from) {
        candidates.extend([
            home.join(format!(".local/bin/{binary_name}")),
            home.join(format!(".volta/bin/{binary_name}")),
            home.join(format!(".asdf/shims/{binary_name}")),
            home.join(format!(".npm-global/bin/{binary_name}")),
            home.join(format!(".bun/bin/{binary_name}")),
        ]);
        candidates.extend(nvm_binary_candidates(&home, binary_name));
    }

    candidates
}

fn nvm_binary_candidates(home: &Path, binary_name: &str) -> Vec<PathBuf> {
    let node_versions_dir = home.join(".nvm/versions/node");
    let Ok(entries) = std::fs::read_dir(node_versions_dir) else {
        return Vec::new();
    };

    let mut candidates = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path().join("bin").join(binary_name))
        .collect::<Vec<_>>();
    candidates.sort_by(|a, b| b.cmp(a));
    candidates
}

fn find_codex_with_user_shell() -> Option<String> {
    find_binary_with_user_shell("codex")
}

fn find_claude_with_user_shell() -> Option<String> {
    find_binary_with_user_shell("claude")
}

fn path_to_string(path: PathBuf) -> String {
    path.to_string_lossy().into_owned()
}

fn find_binary_with_user_shell(binary_name: &str) -> Option<String> {
    let shell = env::var_os("SHELL").unwrap_or_else(|| "/bin/zsh".into());
    let output = StdCommand::new(shell)
        .arg("-lc")
        .arg(format!("command -v {binary_name}"))
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(PathBuf::from)
        .find(|path| is_executable(path))
        .map(path_to_string)
}

fn is_executable(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }

    #[cfg(not(unix))]
    {
        true
    }
}

fn model_for_codex(params: &RequestParams) -> Option<String> {
    selected_codex_model(
        std::env::var(CODEX_MODEL_ENV_VAR).ok(),
        params.model.as_str(),
    )
}

fn selected_codex_model(env_model: Option<String>, request_model: &str) -> Option<String> {
    env_model
        .filter(|model| !model.trim().is_empty())
        .or_else(|| {
            let model = request_model.trim();
            (model.starts_with("gpt-")).then(|| model.to_string())
        })
}

fn model_for_claude(params: &RequestParams) -> Option<String> {
    selected_claude_model(
        std::env::var(CLAUDE_MODEL_ENV_VAR).ok(),
        params.model.as_str(),
    )
}

fn selected_claude_model(env_model: Option<String>, request_model: &str) -> Option<String> {
    env_model
        .map(|model| model.trim().to_string())
        .filter(|model| !model.is_empty())
        .filter(|model| !is_claude_default_model(model))
        .or_else(|| {
            let model = request_model.trim();
            (is_claude_request_model(model) && !is_claude_default_model(model))
                .then(|| model.to_string())
        })
}

fn is_claude_default_model(model: &str) -> bool {
    matches!(
        model.trim().to_ascii_lowercase().as_str(),
        CLAUDE_DEFAULT_MODEL_ID | "claude-default" | "claude" | "default"
    )
}

fn is_claude_request_model(model: &str) -> bool {
    let model = model.trim().to_ascii_lowercase();
    is_claude_default_model(&model)
        || matches!(model.as_str(), "sonnet" | "opus" | "haiku")
        || matches!(model.as_str(), "sonnet[1m]" | "opusplan")
        || model.starts_with("claude-")
}

fn prompt_for_local_agent(
    params: &RequestParams,
    runtime: LocalAgentRuntime,
    model: Option<&str>,
) -> String {
    let mut prompt = String::new();
    prompt.push_str(&format!(
        "{} local agent session context:\n",
        ChannelState::app_name_display()
    ));
    match model {
        Some(model) => {
            prompt.push_str("The ");
            prompt.push_str(runtime.cli_name());
            prompt.push_str(" was invoked with configured model label `");
            prompt.push_str(model);
            prompt.push_str("`.\n");
        }
        None => {
            prompt.push_str("The ");
            prompt.push_str(runtime.cli_name());
            prompt.push_str(
                " was invoked without an explicit `--model` flag, so it is using that CLI's default model.\n",
            )
        }
    }
    prompt.push_str(MODEL_IDENTITY_PROSE);
    prompt.push_str(&tool_call_contract_prose());

    let history = conversation_history(&params.tasks);
    if !history.is_empty() {
        prompt.push_str("Conversation so far:\n");
        prompt.push_str(&history);
        prompt.push_str("\n\n");
    }

    prompt.push_str("User request:\n");
    prompt.push_str(&prompt_from_inputs(&params.input));
    prompt
}

fn tool_call_contract_prose() -> String {
    let app_name = ChannelState::app_name_display();
    format!(
        "{app_name} terminal tool-call contract:\n\
         - {app_name} is a terminal. Prefer making progress with shell commands, scripts, repository inspection, tests, and concise analysis over conversational-only answers.\n\
         - You cannot change {app_name}'s live terminal by changing your own local agent subprocess working directory.\n\
         - When the user asks to run, inspect, analyze, search, test, build, install, execute a script, or change directories, emit one tool-call marker per required shell command on its own line:\n\
         DWARF_TOOL_CALL {{\"type\":\"run_shell_command\",\"command\":\"pwd\",\"is_read_only\":true,\"uses_pager\":false,\"is_risky\":false,\"wait_until_complete\":true}}\n\
         - Emit one self-contained command when a later step depends on an earlier command's output. Do not emit dependent multi-step plans because {app_name} will not feed command results back to you automatically in this local bridge.\n\
         - For directory changes, emit `cd <path>` as a {app_name} tool call. Do not say the cwd changed until {app_name} returns the command result.\n\
         - For read-only inspection commands such as pwd, ls, find, rg, git status, cargo test --no-run, use `is_read_only:true`.\n\
         - For scripts, builds, tests that execute project code, or commands that may modify files, set `is_read_only:false`. Set `is_risky:true` only for destructive, credential, network, sudo, or external side-effect commands.\n\
         - Do not wrap DWARF_TOOL_CALL lines in markdown fences. Keep prose short and do not claim validation from commands that only ran in your local agent subprocess.\n\n"
    )
}

fn conversation_history(tasks: &[api::Task]) -> String {
    let mut entries = Vec::new();
    for task in tasks {
        for message in &task.messages {
            match &message.message {
                Some(api::message::Message::UserQuery(query)) => {
                    entries.push(format!("User: {}", query.query));
                }
                Some(api::message::Message::AgentOutput(output)) => {
                    entries.push(format!("Assistant: {}", output.text));
                }
                _ => {}
            }
        }
    }
    entries.join("\n\n")
}

fn prompt_from_inputs(inputs: &[AIAgentInput]) -> String {
    let parts = inputs
        .iter()
        .filter_map(|input| {
            let text = input.user_query().unwrap_or_else(|| input.to_string());
            (!text.trim().is_empty()).then_some(text)
        })
        .collect::<Vec<_>>();
    if parts.is_empty() {
        "Continue the conversation.".to_string()
    } else {
        parts.join("\n\n")
    }
}

fn user_query_messages(
    inputs: &[AIAgentInput],
    task_id: &str,
    request_id: &str,
) -> Vec<api::Message> {
    inputs
        .iter()
        .filter_map(|input| {
            let query = input.user_query()?;
            Some(api::Message {
                id: format!("local-codex-user-message-{}", Uuid::new_v4()),
                task_id: task_id.to_string(),
                request_id: request_id.to_string(),
                timestamp: None,
                server_message_data: String::new(),
                citations: vec![],
                message: Some(api::message::Message::UserQuery(api::message::UserQuery {
                    query,
                    context: None,
                    referenced_attachments: Default::default(),
                    mode: input.user_query_mode().map(Into::into),
                    intended_agent: Default::default(),
                })),
            })
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LocalRunShellCommand {
    command: String,
    is_read_only: bool,
    uses_pager: bool,
    is_risky: bool,
    wait_until_complete: bool,
}

impl LocalRunShellCommand {
    fn read_only(command: String) -> Self {
        LocalRunShellCommand {
            command,
            is_read_only: true,
            uses_pager: false,
            is_risky: false,
            wait_until_complete: true,
        }
    }

    fn risk_category(&self) -> api::RiskCategory {
        if self.is_risky {
            api::RiskCategory::Risky
        } else if self.is_read_only {
            api::RiskCategory::ReadOnly
        } else {
            api::RiskCategory::TrivialLocalChange
        }
    }
}

#[derive(Deserialize)]
struct DwarfToolCallMarker {
    #[serde(rename = "type")]
    kind: String,
    command: String,
    #[serde(default)]
    is_read_only: Option<bool>,
    #[serde(default)]
    uses_pager: Option<bool>,
    #[serde(default)]
    is_risky: Option<bool>,
    #[serde(default)]
    wait_until_complete: Option<bool>,
}

fn run_shell_command_message(
    task_id: &str,
    request_id: &str,
    shell_command: LocalRunShellCommand,
) -> api::Message {
    let risk_category = shell_command.risk_category() as i32;
    api::Message {
        id: format!("local-codex-tool-message-{}", Uuid::new_v4()),
        task_id: task_id.to_string(),
        request_id: request_id.to_string(),
        timestamp: None,
        server_message_data: String::new(),
        citations: vec![],
        message: Some(api::message::Message::ToolCall(api::message::ToolCall {
            tool_call_id: format!("local-codex-tool-call-{}", Uuid::new_v4()),
            tool: Some(api::message::tool_call::Tool::RunShellCommand(
                api::message::tool_call::RunShellCommand {
                    command: shell_command.command,
                    is_read_only: shell_command.is_read_only,
                    uses_pager: shell_command.uses_pager,
                    citations: vec![],
                    is_risky: shell_command.is_risky,
                    risk_category,
                    wait_until_complete_value: Some(
                        api::message::tool_call::run_shell_command::WaitUntilCompleteValue::WaitUntilComplete(
                            shell_command.wait_until_complete,
                        ),
                    ),
                },
            )),
        })),
    }
}

fn local_tool_output_event(
    task_id: &str,
    request_id: &str,
    output: LocalToolOutputDelta,
) -> api::ResponseEvent {
    if output.is_update {
        update_local_tool_output_event(task_id, request_id, &output)
    } else {
        add_local_tool_output_event(task_id, request_id, &output)
    }
}

fn add_local_tool_output_event(
    task_id: &str,
    request_id: &str,
    output: &LocalToolOutputDelta,
) -> api::ResponseEvent {
    client_actions_event(vec![api::ClientAction {
        action: Some(api::client_action::Action::AddMessagesToTask(
            api::client_action::AddMessagesToTask {
                task_id: task_id.to_string(),
                messages: vec![local_tool_output_message(task_id, request_id, output)],
            },
        )),
    }])
}

fn update_local_tool_output_event(
    task_id: &str,
    request_id: &str,
    output: &LocalToolOutputDelta,
) -> api::ResponseEvent {
    client_actions_event(vec![api::ClientAction {
        action: Some(api::client_action::Action::UpdateTaskMessage(
            api::client_action::UpdateTaskMessage {
                task_id: task_id.to_string(),
                message: Some(local_tool_output_message(task_id, request_id, output)),
                mask: Some(FieldMask {
                    paths: vec![
                        "agent_output.text".to_string(),
                        "server_message_data".to_string(),
                    ],
                }),
            },
        )),
    }])
}

fn local_tool_output_message(
    task_id: &str,
    request_id: &str,
    output: &LocalToolOutputDelta,
) -> api::Message {
    let metadata = LocalCLIToolOutputServerData {
        kind: LOCAL_CLI_TOOL_OUTPUT_SERVER_DATA_TYPE,
        title: &output.title,
        is_complete: output.is_complete,
        is_error: output.is_error,
    };
    api::Message {
        id: local_tool_output_message_id(&output.item_id),
        task_id: task_id.to_string(),
        request_id: request_id.to_string(),
        timestamp: None,
        server_message_data: serde_json::to_string(&metadata).unwrap_or_default(),
        citations: vec![],
        message: Some(api::message::Message::AgentOutput(
            api::message::AgentOutput {
                text: output.body.clone(),
            },
        )),
    }
}

fn local_tool_output_message_id(item_id: &str) -> String {
    if item_id.is_empty() {
        format!("local-codex-tool-output-{}", Uuid::new_v4())
    } else {
        format!("local-codex-tool-output-{item_id}")
    }
}

/// Per-stream line-buffered filter that separates `DWARF_TOOL_CALL` markers from
/// prose deltas as they arrive on the wire. Lets the streaming loop emit clean
/// text to the UI and hoist tool calls into their own blocks *during* streaming,
/// instead of showing raw `DWARF_TOOL_CALL {...}` JSON to the user and replacing
/// it after the stream ends.
#[derive(Default)]
struct StreamingToolCallFilter {
    /// Accumulating bytes whose containing line hasn't terminated yet — we only
    /// know if a line is a marker once we see its trailing `\n`.
    pending_line: String,
}

#[derive(Debug, Default)]
struct FilteredDelta {
    /// Prose chunks safe to emit as `AppendToMessageContent` text deltas.
    text_chunks: Vec<String>,
    /// Tool calls whose marker line just completed.
    tool_calls: Vec<LocalRunShellCommand>,
}

impl StreamingToolCallFilter {
    /// Append a chunk from the CLI stream and pull out any complete lines.
    fn ingest(&mut self, chunk: &str) -> FilteredDelta {
        let mut result = FilteredDelta::default();
        self.pending_line.push_str(chunk);

        self.drain_complete_lines(&mut result);
        if !self.pending_line.is_empty() && !is_possible_marker_fragment(&self.pending_line) {
            result
                .text_chunks
                .push(std::mem::take(&mut self.pending_line));
        }

        result
    }

    /// Drain complete lines from `pending_line`. A "line" here is everything up
    /// to and including the `\n`. Anything after the last `\n` stays buffered
    /// only long enough to decide whether it could be a tool-call marker.
    fn drain_complete_lines(&mut self, result: &mut FilteredDelta) {
        while let Some(nl_pos) = self.pending_line.find('\n') {
            let line_with_newline: String = self.pending_line.drain(..=nl_pos).collect();
            let line_without_newline = &line_with_newline[..line_with_newline.len() - 1];

            if let Some(marker) = parse_dwarf_marker_line(line_without_newline) {
                result.tool_calls.push(marker);
            } else {
                result.text_chunks.push(line_with_newline);
            }
        }
    }

    /// Drain whatever's left in the buffer at end-of-stream. If the trailing
    /// line is a complete marker, return it as a tool call; if it looks like a
    /// partial marker (starts with the prefix but no closing brace), drop it
    /// silently rather than show half a JSON object to the user; otherwise
    /// return it as a final text chunk.
    fn flush(mut self) -> FilteredDelta {
        let mut result = FilteredDelta::default();
        if self.pending_line.is_empty() {
            return result;
        }
        if let Some(marker) = parse_dwarf_marker_line(&self.pending_line) {
            result.tool_calls.push(marker);
            return result;
        }
        if self.pending_line.trim_start().starts_with(TOOL_CALL_PREFIX) {
            // Partial/malformed marker — better to drop than expose JSON fragments.
            return result;
        }
        result
            .text_chunks
            .push(std::mem::take(&mut self.pending_line));
        result
    }
}

/// Parse a single line (no trailing newline) as a DWARF_TOOL_CALL marker.
/// Returns `Some(LocalRunShellCommand)` for a valid `run_shell_command` marker
/// with a non-empty `command`. Anything else yields `None` so the caller knows
/// to keep the line as prose.
fn parse_dwarf_marker_line(line: &str) -> Option<LocalRunShellCommand> {
    let json = dwarf_tool_call_json(line)?;
    let marker: DwarfToolCallMarker = serde_json::from_str(json).ok()?;
    if marker.kind != "run_shell_command" || marker.command.trim().is_empty() {
        return None;
    }
    Some(LocalRunShellCommand {
        command: marker.command.trim().to_string(),
        is_read_only: marker.is_read_only.unwrap_or(true),
        uses_pager: marker.uses_pager.unwrap_or(false),
        is_risky: marker.is_risky.unwrap_or(false),
        wait_until_complete: marker.wait_until_complete.unwrap_or(true),
    })
}

fn is_possible_marker_fragment(line: &str) -> bool {
    let trimmed = line.trim_start();
    !trimmed.is_empty()
        && (TOOL_CALL_PREFIX.starts_with(trimmed) || trimmed.starts_with(TOOL_CALL_PREFIX))
}

fn local_agent_stream_error_text(existing_text: &str, error: &str) -> String {
    if existing_text.trim().is_empty() {
        error.to_string()
    } else {
        format!("{}\n\n{}", existing_text.trim_end(), error)
    }
}

/// Whole-string variant of the tool-call extractor — used by tests and as a
/// reference implementation. The live streaming path now uses
/// `StreamingToolCallFilter` directly so markers never reach the UI.
#[allow(dead_code)]
fn extract_dwarf_tool_calls(output_text: &str) -> (String, Vec<LocalRunShellCommand>) {
    // Fast path: no marker substring anywhere — return the input verbatim without
    // a second walk or any allocation beyond the (cheap) `to_owned`.
    if !output_text.contains(TOOL_CALL_PREFIX) {
        return (output_text.trim().to_string(), Vec::new());
    }

    let mut cleaned = String::with_capacity(output_text.len());
    let mut tool_calls = Vec::new();
    let mut first = true;
    for line in output_text.lines() {
        let extracted = dwarf_tool_call_json(line)
            .and_then(|json| serde_json::from_str::<DwarfToolCallMarker>(json).ok())
            .filter(|marker| {
                marker.kind == "run_shell_command" && !marker.command.trim().is_empty()
            });

        if let Some(marker) = extracted {
            tool_calls.push(LocalRunShellCommand {
                command: marker.command.trim().to_string(),
                is_read_only: marker.is_read_only.unwrap_or(true),
                uses_pager: marker.uses_pager.unwrap_or(false),
                is_risky: marker.is_risky.unwrap_or(false),
                wait_until_complete: marker.wait_until_complete.unwrap_or(true),
            });
            continue;
        }

        if !first {
            cleaned.push('\n');
        }
        cleaned.push_str(line);
        first = false;
    }

    let cleaned = cleaned.trim().to_string();
    let cleaned = if cleaned.is_empty() && !tool_calls.is_empty() {
        format!(
            "I'll run the requested command in {}.",
            ChannelState::app_name_display()
        )
    } else {
        cleaned
    };

    (cleaned, tool_calls)
}

fn dwarf_tool_call_json(line: &str) -> Option<&str> {
    let start = line.find(TOOL_CALL_PREFIX)?;
    let rest = line[start + TOOL_CALL_PREFIX.len()..]
        .trim_start()
        .strip_prefix(':')
        .unwrap_or_else(|| line[start + TOOL_CALL_PREFIX.len()..].trim_start())
        .trim_start();
    rest.starts_with('{').then_some(rest)
}

fn direct_terminal_tool_call(
    inputs: &[AIAgentInput],
    working_directory: Option<&str>,
) -> Option<LocalRunShellCommand> {
    let prompt = prompt_from_inputs(inputs);
    if let Some(target_dir) = local_cwd_change_target(inputs, "", working_directory) {
        return Some(LocalRunShellCommand::read_only(format!(
            "cd {}",
            shell_quote_path(&target_dir)
        )));
    }

    find_and_cd_search_target(&prompt)
        .map(|target| LocalRunShellCommand::read_only(find_and_cd_command(&target)))
}

fn local_cwd_change_target(
    inputs: &[AIAgentInput],
    output_text: &str,
    working_directory: Option<&str>,
) -> Option<PathBuf> {
    let prompt = prompt_from_inputs(inputs);
    if !wants_directory_change(&prompt) {
        return None;
    }

    let target_dir = extract_existing_directory_from_text(output_text)
        .or_else(|| extract_existing_directory_from_text(&prompt))?;
    if working_directory
        .map(Path::new)
        .is_some_and(|working_directory| same_directory(working_directory, &target_dir))
    {
        return None;
    }

    Some(target_dir)
}

fn wants_directory_change(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    [
        "cd ",
        "cd to",
        "change cwd",
        "change directory",
        "change working directory",
        "go to ",
        "bring me into",
        "bring me to",
        "bring me there",
        "move cwd",
        "move me to ",
        "move to ",
        "move into",
        "set cwd",
        "set working directory",
        "switch to ",
    ]
    .iter()
    .any(|phrase| text.contains(phrase))
}

fn find_and_cd_search_target(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    if !lower.contains("find") || !wants_directory_change(text) {
        return None;
    }

    let target = extract_between_after_keyword(text, &lower, "where the ", " is")
        .or_else(|| extract_between_after_keyword(text, &lower, "where ", " is"))
        .or_else(|| extract_between_after_keyword(text, &lower, "find the ", " and "))
        .or_else(|| extract_between_after_keyword(text, &lower, "find ", " and "))
        .or_else(|| extract_between_after_keyword(text, &lower, "find ", " then "))
        .or_else(|| extract_after_keyword(text, &lower, "find "))?;
    sanitize_search_target(target)
}

fn extract_between_after_keyword<'a>(
    original: &'a str,
    lower: &str,
    start_keyword: &str,
    end_keyword: &str,
) -> Option<&'a str> {
    let start = lower.find(start_keyword)? + start_keyword.len();
    let rest_lower = &lower[start..];
    let end = rest_lower.find(end_keyword)?;
    Some(&original[start..start + end])
}

fn extract_after_keyword<'a>(original: &'a str, lower: &str, keyword: &str) -> Option<&'a str> {
    let start = lower.find(keyword)? + keyword.len();
    Some(&original[start..])
}

fn sanitize_search_target(target: &str) -> Option<String> {
    let target = target
        .trim()
        .trim_matches(|char: char| char.is_ascii_punctuation() || char.is_whitespace());
    let target = target
        .strip_prefix("project ")
        .or_else(|| target.strip_prefix("folder "))
        .or_else(|| target.strip_prefix("directory "))
        .unwrap_or(target)
        .trim();

    if target.is_empty()
        || !target
            .chars()
            .all(|char| char.is_ascii_alphanumeric() || matches!(char, '.' | '_' | '-' | ' '))
    {
        return None;
    }

    Some(target.to_string())
}

fn find_and_cd_command(target: &str) -> String {
    let pattern = format!("*{target}*");
    let mdfind_query = format!("kMDItemFSName == \"{pattern}\"c");
    let no_match_message = format!("No matching directory found for {target}");

    format!(
        "target=$(mdfind {} 2>/dev/null | while IFS= read -r path; do [ -d \"$path\" ] && printf '%s\\n' \"$path\" && break; done); if [ -z \"$target\" ]; then target=$(find \"$HOME/Documents\" \"$HOME/Downloads\" \"$HOME/Desktop\" \"$HOME\" -iname {} -type d -print -quit 2>/dev/null); fi; if [ -n \"$target\" ]; then cd \"$target\" && pwd; else echo {} >&2; false; fi",
        shell_quote(&mdfind_query),
        shell_quote(&pattern),
        shell_quote(&no_match_message),
    )
}

fn extract_existing_directory_from_text(text: &str) -> Option<PathBuf> {
    text.lines()
        .flat_map(candidate_paths_from_line)
        .find(|path| path.is_dir())
}

fn candidate_paths_from_line(line: &str) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    let line_candidate = trim_path_candidate(line);
    if let Some(path) = candidate_path(line_candidate) {
        candidates.push(path);
    }

    candidates.extend(
        line.split_whitespace()
            .map(trim_path_candidate)
            .filter_map(candidate_path),
    );

    candidates
}

fn candidate_path(candidate: &str) -> Option<PathBuf> {
    if candidate.starts_with('/') {
        return Some(PathBuf::from(candidate));
    }

    let rest = candidate.strip_prefix("~/")?;
    env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(rest))
}

fn trim_path_candidate(value: &str) -> &str {
    value
        .trim_matches(|char: char| {
            char.is_whitespace()
                || matches!(char, '`' | '"' | '\'' | '<' | '>' | '(' | ')' | '[' | ']')
        })
        .trim_end_matches(|char: char| matches!(char, '.' | ',' | ';' | ':' | '!' | '?'))
}

fn same_directory(left: &Path, right: &Path) -> bool {
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

fn shell_quote_path(path: &Path) -> String {
    let path = path.to_string_lossy();
    shell_quote(&path)
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn root_task_id(tasks: &[api::Task]) -> Option<String> {
    tasks
        .iter()
        .find(|task| {
            task.dependencies
                .as_ref()
                .is_none_or(|dependencies| dependencies.parent_task_id.is_empty())
        })
        .map(|task| task.id.clone())
}

fn init_event(conversation_id: String, request_id: String) -> api::ResponseEvent {
    api::ResponseEvent {
        r#type: Some(api::response_event::Type::Init(
            api::response_event::StreamInit {
                conversation_id,
                request_id,
                run_id: String::new(),
            },
        )),
    }
}

fn client_actions_event(actions: Vec<api::ClientAction>) -> api::ResponseEvent {
    api::ResponseEvent {
        r#type: Some(api::response_event::Type::ClientActions(
            api::response_event::ClientActions { actions },
        )),
    }
}

fn add_or_append_agent_text_event(
    task_id: &str,
    request_id: &str,
    runtime: LocalAgentRuntime,
    current_message_id: &mut Option<String>,
    text: String,
) -> api::ResponseEvent {
    if let Some(message_id) = current_message_id.as_deref() {
        append_agent_text_event(task_id, message_id, text)
    } else {
        let message_id = format!("local-{}-message-{}", runtime.slug(), Uuid::new_v4());
        *current_message_id = Some(message_id.clone());
        add_agent_text_event(task_id, request_id, message_id, text)
    }
}

fn add_agent_text_event(
    task_id: &str,
    request_id: &str,
    message_id: String,
    text: String,
) -> api::ResponseEvent {
    client_actions_event(vec![api::ClientAction {
        action: Some(api::client_action::Action::AddMessagesToTask(
            api::client_action::AddMessagesToTask {
                task_id: task_id.to_string(),
                messages: vec![api::Message {
                    id: message_id,
                    task_id: task_id.to_string(),
                    request_id: request_id.to_string(),
                    timestamp: None,
                    server_message_data: String::new(),
                    citations: vec![],
                    message: Some(api::message::Message::AgentOutput(
                        api::message::AgentOutput { text },
                    )),
                }],
            },
        )),
    }])
}

fn append_agent_text_event(task_id: &str, message_id: &str, text: String) -> api::ResponseEvent {
    client_actions_event(vec![api::ClientAction {
        action: Some(api::client_action::Action::AppendToMessageContent(
            api::client_action::AppendToMessageContent {
                task_id: task_id.to_string(),
                message: Some(api::Message {
                    id: message_id.to_string(),
                    task_id: task_id.to_string(),
                    request_id: String::new(),
                    timestamp: None,
                    server_message_data: String::new(),
                    citations: vec![],
                    message: Some(api::message::Message::AgentOutput(
                        api::message::AgentOutput { text },
                    )),
                }),
                mask: Some(FieldMask {
                    paths: vec!["agent_output.text".to_string()],
                }),
            },
        )),
    }])
}

fn replace_agent_text_event(task_id: &str, message_id: &str, text: String) -> api::ResponseEvent {
    // request_id on api::Message is metadata the conversation handler doesn't consult
    // for UpdateTaskMessage (lookup is by message_id), so leave it empty.
    client_actions_event(vec![api::ClientAction {
        action: Some(api::client_action::Action::UpdateTaskMessage(
            api::client_action::UpdateTaskMessage {
                task_id: task_id.to_string(),
                message: Some(api::Message {
                    id: message_id.to_string(),
                    task_id: task_id.to_string(),
                    request_id: String::new(),
                    timestamp: None,
                    server_message_data: String::new(),
                    citations: vec![],
                    message: Some(api::message::Message::AgentOutput(
                        api::message::AgentOutput { text },
                    )),
                }),
                mask: Some(FieldMask {
                    paths: vec!["agent_output.text".to_string()],
                }),
            },
        )),
    }])
}

fn finished_event() -> api::ResponseEvent {
    #[allow(deprecated)]
    let conversation_usage_metadata =
        api::response_event::stream_finished::ConversationUsageMetadata {
            context_window_usage: 0.0,
            summarized: false,
            credits_spent: 0.0,
            token_usage: vec![],
            tool_usage_metadata: None,
            warp_token_usage: Default::default(),
            byok_token_usage: Default::default(),
        };

    api::ResponseEvent {
        r#type: Some(api::response_event::Type::Finished(
            api::response_event::StreamFinished {
                reason: Some(api::response_event::stream_finished::Reason::Done(
                    api::response_event::stream_finished::Done {},
                )),
                token_usage: vec![],
                should_refresh_model_config: false,
                request_cost: None,
                conversation_usage_metadata: Some(conversation_usage_metadata),
            },
        )),
    }
}

fn extract_claude_agent_text(stdout: &str) -> Option<String> {
    let stdout = stdout.trim();
    if stdout.is_empty() {
        return None;
    }

    let Ok(value) = serde_json::from_str::<Value>(stdout) else {
        return Some(stdout.to_string());
    };

    for key in ["result", "response", "text", "message"] {
        if let Some(text) = value
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
        {
            return Some(text.to_string());
        }
    }

    value
        .get("content")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n\n")
        })
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

fn truncate(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let truncated = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{truncated}\n...")
    } else {
        truncated
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use field_mask::FieldMaskOperation;

    fn test_agent_output_message(text: &str) -> api::Message {
        api::Message {
            id: "message".to_string(),
            task_id: "task".to_string(),
            request_id: "request".to_string(),
            timestamp: None,
            server_message_data: String::new(),
            citations: vec![],
            message: Some(api::message::Message::AgentOutput(
                api::message::AgentOutput {
                    text: text.to_string(),
                },
            )),
        }
    }

    fn agent_output_text(message: &api::Message) -> &str {
        let Some(api::message::Message::AgentOutput(output)) = message.message.as_ref() else {
            panic!("expected agent output message");
        };
        &output.text
    }

    #[test]
    fn local_agent_text_events_update_agent_output_text() {
        let existing = test_agent_output_message("hel");
        let api::response_event::Type::ClientActions(actions) =
            append_agent_text_event("task", "message", "lo".to_string())
                .r#type
                .expect("append event type")
        else {
            panic!("expected client actions");
        };
        let api::client_action::Action::AppendToMessageContent(append) = actions
            .actions
            .into_iter()
            .next()
            .expect("append action")
            .action
            .expect("append action type")
        else {
            panic!("expected append action");
        };
        let appended = FieldMaskOperation::append(
            &api::MESSAGE_DESCRIPTOR,
            &existing,
            append.message.as_ref().expect("append message"),
            append.mask.expect("append mask"),
        )
        .apply()
        .expect("append mask applies");
        assert_eq!(agent_output_text(&appended), "hello");

        let api::response_event::Type::ClientActions(actions) =
            replace_agent_text_event("task", "message", "replaced".to_string())
                .r#type
                .expect("replace event type")
        else {
            panic!("expected client actions");
        };
        let api::client_action::Action::UpdateTaskMessage(update) = actions
            .actions
            .into_iter()
            .next()
            .expect("update action")
            .action
            .expect("update action type")
        else {
            panic!("expected update action");
        };
        let replaced = FieldMaskOperation::update(
            &api::MESSAGE_DESCRIPTOR,
            &existing,
            update.message.as_ref().expect("update message"),
            update.mask.expect("update mask"),
        )
        .apply()
        .expect("update mask applies");
        assert_eq!(agent_output_text(&replaced), "replaced");
    }

    #[test]
    fn local_tool_output_events_update_metadata_and_body() {
        let started = LocalToolOutputDelta {
            item_id: "item_0".to_string(),
            title: "Running pwd".to_string(),
            body: String::new(),
            is_complete: false,
            is_error: false,
            is_update: false,
        };
        let api::response_event::Type::ClientActions(actions) =
            local_tool_output_event("task", "request", started)
                .r#type
                .expect("add event type")
        else {
            panic!("expected client actions");
        };
        let api::client_action::Action::AddMessagesToTask(add) = actions
            .actions
            .into_iter()
            .next()
            .expect("add action")
            .action
            .expect("add action type")
        else {
            panic!("expected add action");
        };
        let existing = add.messages.into_iter().next().expect("added message");
        assert_eq!(existing.id, "local-codex-tool-output-item_0");
        assert_eq!(agent_output_text(&existing), "");
        let metadata: Value =
            serde_json::from_str(&existing.server_message_data).expect("valid server data");
        assert_eq!(metadata["title"], "Running pwd");
        assert_eq!(metadata["is_complete"], false);

        let completed = LocalToolOutputDelta {
            item_id: "item_0".to_string(),
            title: "Ran pwd".to_string(),
            body: "/tmp".to_string(),
            is_complete: true,
            is_error: false,
            is_update: true,
        };
        let api::response_event::Type::ClientActions(actions) =
            local_tool_output_event("task", "request", completed)
                .r#type
                .expect("update event type")
        else {
            panic!("expected client actions");
        };
        let api::client_action::Action::UpdateTaskMessage(update) = actions
            .actions
            .into_iter()
            .next()
            .expect("update action")
            .action
            .expect("update action type")
        else {
            panic!("expected update action");
        };
        let updated = FieldMaskOperation::update(
            &api::MESSAGE_DESCRIPTOR,
            &existing,
            update.message.as_ref().expect("update message"),
            update.mask.expect("update mask"),
        )
        .apply()
        .expect("update mask applies");
        assert_eq!(agent_output_text(&updated), "/tmp");
        let metadata: Value =
            serde_json::from_str(&updated.server_message_data).expect("valid server data");
        assert_eq!(metadata["title"], "Ran pwd");
        assert_eq!(metadata["is_complete"], true);
    }

    fn drive_codex(acc: &mut CodexAccumulator, lines: &[&str]) -> String {
        let mut total = String::new();
        for line in lines {
            if let Some(LocalAgentStreamDelta::Text(delta)) = acc.ingest_line(line) {
                total.push_str(&delta);
            }
        }
        total
    }

    fn drive_codex_events(
        acc: &mut CodexAccumulator,
        lines: &[&str],
    ) -> Vec<LocalAgentStreamDelta> {
        lines
            .iter()
            .filter_map(|line| acc.ingest_line(line))
            .collect()
    }

    fn codex_text_delta(text: &str) -> Option<LocalAgentStreamDelta> {
        Some(LocalAgentStreamDelta::Text(text.to_string()))
    }

    fn local_tool_output_delta(
        item_id: &str,
        title: &str,
        body: &str,
        is_complete: bool,
        is_error: bool,
        is_update: bool,
    ) -> LocalAgentStreamDelta {
        LocalAgentStreamDelta::LocalToolOutput(LocalToolOutputDelta {
            item_id: item_id.to_string(),
            title: title.to_string(),
            body: body.to_string(),
            is_complete,
            is_error,
            is_update,
        })
    }

    fn drive_claude(acc: &mut ClaudeAccumulator, lines: &[&str]) -> String {
        let mut total = String::new();
        for line in lines {
            for delta in acc.ingest_line(line) {
                if let LocalAgentStreamDelta::Text(text) = delta {
                    total.push_str(&text);
                }
            }
        }
        total
    }

    fn drive_claude_events(
        acc: &mut ClaudeAccumulator,
        lines: &[&str],
    ) -> Vec<LocalAgentStreamDelta> {
        lines
            .iter()
            .flat_map(|line| acc.ingest_line(line))
            .collect()
    }

    #[test]
    fn ingests_codex_agent_message_jsonl_deltas() {
        let mut acc = CodexAccumulator::default();
        assert_eq!(
            acc.ingest_line(r#"{"type":"thread.started","thread_id":"t"}"#),
            None
        );
        assert_eq!(
            acc.ingest_line(
                r#"{"type":"item.completed","item":{"id":"item_0","type":"agent_message","text":"hel"}}"#,
            ),
            codex_text_delta("hel")
        );
        assert_eq!(
            acc.ingest_line(
                r#"{"type":"item.updated","item":{"id":"item_0","type":"agent_message","text":"hello"}}"#,
            ),
            codex_text_delta("lo")
        );
        assert_eq!(
            acc.ingest_line(
                r#"{"type":"item.completed","item":{"id":"item_1","type":"agent_message","text":"world"}}"#,
            ),
            codex_text_delta("\n\nworld")
        );
        assert!(acc.has_emitted_any);
    }

    #[test]
    fn codex_accumulator_collects_reasoning_summaries_and_item_type_counts() {
        let mut acc = CodexAccumulator::default();
        // Codex emits some non-agent_message items (reasoning, tool_call, etc.) without
        // ever producing an agent_message. We want to (a) skip them in the delta stream
        // but (b) record them for diagnostics + fallback display.
        let total = drive_codex(
            &mut acc,
            &[
                r#"{"type":"item.completed","item":{"id":"r1","type":"reasoning","summary":"I'll check the model module."}}"#,
                r#"{"type":"item.completed","item":{"id":"tool_0","type":"function_call","name":"read_file"}}"#,
                r#"{"type":"item.completed","item":{"id":"r2","type":"reasoning","summary":"That file is large."}}"#,
            ],
        );
        assert!(total.is_empty(), "deltas should not include sideband items");
        assert!(!acc.has_emitted_any);
        assert_eq!(
            acc.reasoning_summaries,
            vec![
                "I'll check the model module.".to_string(),
                "That file is large.".to_string(),
            ]
        );
        assert_eq!(acc.non_message_item_counts.get("reasoning"), Some(&2));
        assert_eq!(acc.non_message_item_counts.get("function_call"), Some(&1));
    }

    #[test]
    fn codex_accumulator_skips_non_agent_items() {
        let mut acc = CodexAccumulator::default();
        let total = drive_codex(
            &mut acc,
            &[
                r#"{"type":"item.completed","item":{"id":"tool_0","type":"tool_call","name":"shell"}}"#,
            ],
        );
        assert!(total.is_empty());
        assert!(!acc.has_emitted_any);
        assert!(acc.texts_by_id.is_empty());
    }

    #[test]
    fn codex_accumulator_ignores_no_op_updates() {
        let mut acc = CodexAccumulator::default();
        let total = drive_codex(
            &mut acc,
            &[
                r#"{"type":"item.started","item":{"id":"m1","type":"agent_message","text":"hello"}}"#,
                r#"{"type":"item.completed","item":{"id":"m1","type":"agent_message","text":"hello"}}"#,
            ],
        );
        assert_eq!(total, "hello");
    }

    #[test]
    fn codex_accumulator_two_messages_in_sequence() {
        let mut acc = CodexAccumulator::default();
        let total = drive_codex(
            &mut acc,
            &[
                r#"{"type":"item.completed","item":{"id":"m1","type":"agent_message","text":"first"}}"#,
                r#"{"type":"item.completed","item":{"id":"m2","type":"agent_message","text":"second"}}"#,
            ],
        );
        assert_eq!(total, "first\n\nsecond");
    }

    #[test]
    fn codex_accumulator_narrates_command_execution_lifecycle() {
        let mut acc = CodexAccumulator::default();
        // Real Codex 0.130 sequence: item.started fires when the sandbox shell
        // launches; item.completed lands with the combined output + exit code.
        let events = drive_codex_events(
            &mut acc,
            &[
                r#"{"type":"item.started","item":{"id":"item_0","type":"command_execution","command":"/bin/zsh -lc pwd","aggregated_output":"","exit_code":null,"status":"in_progress"}}"#,
                r#"{"type":"item.completed","item":{"id":"item_0","type":"command_execution","command":"/bin/zsh -lc pwd","aggregated_output":"/tmp\n","exit_code":0,"status":"completed"}}"#,
            ],
        );
        assert_eq!(
            events,
            vec![
                local_tool_output_delta("item_0", "Running pwd", "", false, false, false),
                local_tool_output_delta("item_0", "Ran pwd", "/tmp", true, false, true),
            ]
        );
    }

    #[test]
    fn codex_accumulator_appends_exit_code_to_failures() {
        let mut acc = CodexAccumulator::default();
        let events = drive_codex_events(
            &mut acc,
            &[
                r#"{"type":"item.started","item":{"id":"item_0","type":"command_execution","command":"/bin/zsh -lc 'false'","aggregated_output":"","exit_code":null,"status":"in_progress"}}"#,
                r#"{"type":"item.completed","item":{"id":"item_0","type":"command_execution","command":"/bin/zsh -lc 'false'","aggregated_output":"","exit_code":1,"status":"completed"}}"#,
            ],
        );
        assert_eq!(
            events,
            vec![
                local_tool_output_delta("item_0", "Running false", "", false, false, false),
                local_tool_output_delta("item_0", "Ran false", "(exit code 1)", true, true, true),
            ]
        );
    }

    #[test]
    fn codex_accumulator_interleaves_tool_calls_and_text() {
        // Real shape: command_execution → agent_message → command_execution → agent_message.
        let mut acc = CodexAccumulator::default();
        let events = drive_codex_events(
            &mut acc,
            &[
                r#"{"type":"item.started","item":{"id":"i0","type":"command_execution","command":"/bin/zsh -lc 'pwd'","aggregated_output":"","exit_code":null,"status":"in_progress"}}"#,
                r#"{"type":"item.completed","item":{"id":"i0","type":"command_execution","command":"/bin/zsh -lc 'pwd'","aggregated_output":"/tmp","exit_code":0,"status":"completed"}}"#,
                r#"{"type":"item.completed","item":{"id":"i1","type":"agent_message","text":"You are in /tmp."}}"#,
                r#"{"type":"item.started","item":{"id":"i2","type":"command_execution","command":"/bin/zsh -lc 'ls'","aggregated_output":"","exit_code":null,"status":"in_progress"}}"#,
                r#"{"type":"item.completed","item":{"id":"i2","type":"command_execution","command":"/bin/zsh -lc 'ls'","aggregated_output":"a.txt\nb.txt","exit_code":0,"status":"completed"}}"#,
                r#"{"type":"item.completed","item":{"id":"i3","type":"agent_message","text":"Two files."}}"#,
            ],
        );
        assert_eq!(
            events,
            vec![
                local_tool_output_delta("i0", "Running pwd", "", false, false, false),
                local_tool_output_delta("i0", "Ran pwd", "/tmp", true, false, true),
                LocalAgentStreamDelta::Text("You are in /tmp.".to_string()),
                local_tool_output_delta("i2", "Running ls", "", false, false, false),
                local_tool_output_delta("i2", "Ran ls", "a.txt\nb.txt", true, false, true),
                LocalAgentStreamDelta::Text("Two files.".to_string()),
            ]
        );
    }

    #[test]
    fn codex_accumulator_narrates_web_search_on_completion() {
        let mut acc = CodexAccumulator::default();
        let events = drive_codex_events(
            &mut acc,
            &[
                r#"{"type":"item.started","item":{"id":"ws_0","type":"web_search","query":"","action":{"type":"other"}}}"#,
                r#"{"type":"item.completed","item":{"id":"ws_0","type":"web_search","query":"current date Seoul","action":{"type":"search","query":"current date Seoul"}}}"#,
                r#"{"type":"item.completed","item":{"id":"m0","type":"agent_message","text":"It's May 19."}}"#,
            ],
        );
        assert_eq!(
            events,
            vec![
                local_tool_output_delta(
                    "ws_0",
                    "Searched web for current date Seoul",
                    "",
                    true,
                    false,
                    false,
                ),
                LocalAgentStreamDelta::Text("It's May 19.".to_string()),
            ]
        );
    }

    #[test]
    fn codex_accumulator_skips_started_when_completion_has_no_started() {
        // Defensive: if we somehow miss the started event, the completion still
        // shows what ran so the chat isn't a mystery.
        let mut acc = CodexAccumulator::default();
        let events = drive_codex_events(
            &mut acc,
            &[
                r#"{"type":"item.completed","item":{"id":"i0","type":"command_execution","command":"/bin/zsh -lc 'pwd'","aggregated_output":"/tmp","exit_code":0,"status":"completed"}}"#,
            ],
        );
        assert_eq!(
            events,
            vec![local_tool_output_delta(
                "i0", "Ran pwd", "/tmp", true, false, false
            )]
        );
    }

    #[test]
    fn strip_shell_wrapper_unwraps_zsh_lc_and_quotes() {
        assert_eq!(strip_shell_wrapper("/bin/zsh -lc pwd"), "pwd");
        assert_eq!(strip_shell_wrapper("/bin/zsh -lc 'ls -la'"), "ls -la");
        assert_eq!(
            strip_shell_wrapper("/bin/bash -lc \"cat /etc/hosts\""),
            "cat /etc/hosts"
        );
        assert_eq!(strip_shell_wrapper("/bin/sh -c 'echo hi'"), "echo hi");
        // No wrapper → pass through.
        assert_eq!(strip_shell_wrapper("git status"), "git status");
    }

    #[test]
    fn ingests_claude_stream_json_deltas() {
        let mut acc = ClaudeAccumulator::default();
        let total = drive_claude(
            &mut acc,
            &[
                r#"{"type":"system","subtype":"init"}"#,
                r#"{"type":"assistant","message":{"content":[{"type":"text","text":"hello"}]}}"#,
                r#"{"type":"assistant","message":{"content":[{"type":"text","text":"world"}]}}"#,
            ],
        );
        assert_eq!(total, "hello\n\nworld");
    }

    #[test]
    fn claude_accumulator_streams_per_token_via_content_block_delta() {
        let mut acc = ClaudeAccumulator::default();
        // Simulate the real Claude `--include-partial-messages` event sequence:
        // message_start → content_block_start → content_block_delta* → assistant → ...
        // The deltas are the source of truth; the trailing `assistant` event repeats
        // the same text and must be suppressed.
        let total = drive_claude(
            &mut acc,
            &[
                r#"{"type":"system","subtype":"init"}"#,
                r#"{"type":"stream_event","event":{"type":"message_start","message":{}}}"#,
                r#"{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}}"#,
                r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"One"}}}"#,
                r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"\nTwo\nThree"}}}"#,
                r#"{"type":"assistant","message":{"content":[{"type":"text","text":"One\nTwo\nThree"}]}}"#,
                r#"{"type":"stream_event","event":{"type":"content_block_stop","index":0}}"#,
                r#"{"type":"result","result":"One\nTwo\nThree"}"#,
            ],
        );
        // Total should be the per-token concatenation, NOT doubled by the assistant or result.
        assert_eq!(total, "One\nTwo\nThree");
        assert!(acc.has_streamed_partials);
    }

    #[test]
    fn claude_accumulator_falls_back_to_result_when_no_assistant_event() {
        let mut acc = ClaudeAccumulator::default();
        let total = drive_claude(
            &mut acc,
            &[
                r#"{"type":"system","subtype":"init"}"#,
                r#"{"type":"result","result":"final answer"}"#,
            ],
        );
        assert_eq!(total, "final answer");
    }

    #[test]
    fn claude_accumulator_skips_result_when_assistant_already_streamed() {
        let mut acc = ClaudeAccumulator::default();
        let total = drive_claude(
            &mut acc,
            &[
                r#"{"type":"assistant","message":{"content":[{"type":"text","text":"hi"}]}}"#,
                r#"{"type":"result","result":"hi"}"#,
            ],
        );
        assert_eq!(total, "hi");
    }

    #[test]
    fn claude_accumulator_renders_bash_tool_use_inline() {
        // Real `--include-partial-messages` shape for a Bash tool call:
        // content_block_start (tool_use, name=Bash) → input_json_delta+ → stop.
        let mut acc = ClaudeAccumulator::default();
        let events = drive_claude_events(
            &mut acc,
            &[
                r#"{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}}"#,
                r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Listing files."}}}"#,
                r#"{"type":"stream_event","event":{"type":"content_block_stop","index":0}}"#,
                r#"{"type":"stream_event","event":{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_abc","name":"Bash","input":{}}}}"#,
                r#"{"type":"stream_event","event":{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"command\": \"ls"}}}"#,
                r#"{"type":"stream_event","event":{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":" -la /tmp\"}"}}}"#,
                r#"{"type":"stream_event","event":{"type":"content_block_stop","index":1}}"#,
            ],
        );
        assert_eq!(
            events,
            vec![
                LocalAgentStreamDelta::Text("Listing files.".to_string()),
                local_tool_output_delta(
                    "toolu_abc",
                    "Running ls -la /tmp",
                    "",
                    false,
                    false,
                    false,
                ),
            ]
        );
    }

    #[test]
    fn claude_accumulator_renders_read_and_edit_tools_inline() {
        let mut acc = ClaudeAccumulator::default();
        let events = drive_claude_events(
            &mut acc,
            &[
                r#"{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"Read","input":{}}}}"#,
                r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"file_path\":\"src/foo.rs\"}"}}}"#,
                r#"{"type":"stream_event","event":{"type":"content_block_stop","index":0}}"#,
                r#"{"type":"stream_event","event":{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_2","name":"Edit","input":{}}}}"#,
                r#"{"type":"stream_event","event":{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"file_path\":\"src/bar.rs\"}"}}}"#,
                r#"{"type":"stream_event","event":{"type":"content_block_stop","index":1}}"#,
            ],
        );
        assert_eq!(
            events,
            vec![
                local_tool_output_delta("toolu_1", "Reading src/foo.rs", "", false, false, false,),
                local_tool_output_delta("toolu_2", "Editing src/bar.rs", "", false, false, false,),
            ]
        );
    }

    #[test]
    fn claude_accumulator_renders_tool_result_user_event() {
        let mut acc = ClaudeAccumulator::default();
        let events = drive_claude_events(
            &mut acc,
            &[
                r#"{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_abc","name":"Bash","input":{}}}}"#,
                r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"command\":\"echo hi\"}"}}}"#,
                r#"{"type":"stream_event","event":{"type":"content_block_stop","index":0}}"#,
                r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_abc","content":"hi\n","is_error":false}]}}"#,
            ],
        );
        assert_eq!(
            events,
            vec![
                local_tool_output_delta("toolu_abc", "Running echo hi", "", false, false, false,),
                local_tool_output_delta("toolu_abc", "Ran echo hi", "hi", true, false, true),
            ]
        );
    }

    #[test]
    fn claude_accumulator_truncates_long_tool_results() {
        let mut acc = ClaudeAccumulator::default();
        let lines: Vec<String> = (0..50).map(|i| format!("line {i}")).collect();
        let big = lines.join("\\n"); // escape for the JSON string body
        let user_event = format!(
            r#"{{"type":"user","message":{{"content":[{{"type":"tool_result","tool_use_id":"x","content":"{big}"}}]}}}}"#
        );
        let events = drive_claude_events(&mut acc, &[user_event.as_str()]);
        let [LocalAgentStreamDelta::LocalToolOutput(output)] = events.as_slice() else {
            panic!("expected one local tool output event");
        };
        // Truncated to MAX_LINES (12) with an ellipsis marker.
        assert_eq!(output.title, "Used tool");
        assert!(output.body.starts_with("line 0\nline 1\n"));
        assert!(output.body.contains("line 11"));
        assert!(output.body.ends_with("..."));
        assert!(!output.body.contains("line 12"));
    }

    #[test]
    fn claude_accumulator_renders_todowrite_as_checklist() {
        let mut acc = ClaudeAccumulator::default();
        let events = drive_claude_events(
            &mut acc,
            &[
                r#"{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_t","name":"TodoWrite","input":{}}}}"#,
                r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"todos\":[{\"content\":\"Read docs\",\"status\":\"completed\"},{\"content\":\"Run tests\",\"status\":\"in_progress\"},{\"content\":\"Ship it\",\"status\":\"pending\"}]}"}}}"#,
                r#"{"type":"stream_event","event":{"type":"content_block_stop","index":0}}"#,
            ],
        );
        assert_eq!(
            events,
            vec![local_tool_output_delta(
                "toolu_t",
                "Updated todos",
                "- [x] Read docs\n- [~] Run tests\n- [ ] Ship it",
                true,
                false,
                false,
            )]
        );
    }

    #[test]
    fn claude_accumulator_handles_tool_result_array_content() {
        // Claude sometimes sends tool_result.content as an array of {type:"text",text:...} parts.
        let mut acc = ClaudeAccumulator::default();
        let events = drive_claude_events(
            &mut acc,
            &[
                r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"x","content":[{"type":"text","text":"part a"},{"type":"text","text":"part b"}]}]}}"#,
            ],
        );
        assert_eq!(
            events,
            vec![local_tool_output_delta(
                "x",
                "Used tool",
                "part a\npart b",
                true,
                false,
                false,
            )]
        );
    }

    #[test]
    fn claude_accumulator_does_not_prefix_text_after_structured_tool_output() {
        let mut acc = ClaudeAccumulator::default();
        let events = drive_claude_events(
            &mut acc,
            &[
                r#"{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_abc","name":"Bash","input":{}}}}"#,
                r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"command\":\"pwd\"}"}}}"#,
                r#"{"type":"stream_event","event":{"type":"content_block_stop","index":0}}"#,
                r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_abc","content":"/tmp\n","is_error":false}]}}"#,
                r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Done."}]}}"#,
            ],
        );
        assert_eq!(
            events,
            vec![
                local_tool_output_delta("toolu_abc", "Running pwd", "", false, false, false,),
                local_tool_output_delta("toolu_abc", "Ran pwd", "/tmp", true, false, true),
                LocalAgentStreamDelta::Text("Done.".to_string()),
            ]
        );
    }

    #[test]
    fn claude_accumulator_keeps_tool_result_markdown_out_of_text_stream() {
        let mut acc = ClaudeAccumulator::default();
        let events = drive_claude_events(
            &mut acc,
            &[
                r#"{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_grep","name":"Grep","input":{}}}}"#,
                r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"pattern\":\"```\"}"}}}"#,
                r#"{"type":"stream_event","event":{"type":"content_block_stop","index":0}}"#,
                r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_grep","content":"found ``` fence\n## heading\n","is_error":false}]}}"#,
                r#"{"type":"stream_event","event":{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}}"#,
                r###"{"type":"stream_event","event":{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"## Final answer"}}}"###,
            ],
        );
        assert_eq!(
            events,
            vec![
                local_tool_output_delta("toolu_grep", "Grepping for ```", "", false, false, false,),
                local_tool_output_delta(
                    "toolu_grep",
                    "Grepped for ```",
                    "found ``` fence\n## heading",
                    true,
                    false,
                    true,
                ),
                LocalAgentStreamDelta::Text("## Final answer".to_string()),
            ]
        );
    }

    #[test]
    fn local_agent_timeout_defaults_when_env_unset() {
        // Don't mutate env in tests; just sanity-check the default path returns the
        // documented fallback when the env var is absent / unparseable.
        env::remove_var(LOCAL_AGENT_TIMEOUT_ENV_VAR);
        assert_eq!(
            local_agent_timeout(),
            Duration::from_secs(DEFAULT_LOCAL_AGENT_TIMEOUT_SECS)
        );

        env::set_var(LOCAL_AGENT_TIMEOUT_ENV_VAR, "not-a-number");
        assert_eq!(
            local_agent_timeout(),
            Duration::from_secs(DEFAULT_LOCAL_AGENT_TIMEOUT_SECS)
        );

        env::set_var(LOCAL_AGENT_TIMEOUT_ENV_VAR, "0");
        assert_eq!(
            local_agent_timeout(),
            Duration::from_secs(DEFAULT_LOCAL_AGENT_TIMEOUT_SECS),
            "zero should fall back to the default to avoid instant-timeout footgun"
        );

        env::set_var(LOCAL_AGENT_TIMEOUT_ENV_VAR, "42");
        assert_eq!(local_agent_timeout(), Duration::from_secs(42));

        env::remove_var(LOCAL_AGENT_TIMEOUT_ENV_VAR);
    }

    #[test]
    fn streaming_filter_separates_markers_from_prose() {
        let mut filter = StreamingToolCallFilter::default();
        // Chunk 1: one prose line + start of a marker that crosses chunk boundary.
        let r1 = filter.ingest("I'll inspect the repo.\nDWARF_TOOL_CALL {\"type");
        assert_eq!(r1.text_chunks, vec!["I'll inspect the repo.\n".to_string()]);
        assert!(r1.tool_calls.is_empty());

        // Chunk 2: rest of the marker + start of another marker.
        let r2 = filter.ingest("\":\"run_shell_command\",\"command\":\"ls -la\",\"is_read_only\":true}\nDWARF_TOOL_CALL {\"type\":\"run_shell_command\",\"command\":\"cat README.md\",\"is_read_only\":true}\n");
        assert!(r2.text_chunks.is_empty(), "no prose between markers");
        assert_eq!(r2.tool_calls.len(), 2);
        assert_eq!(r2.tool_calls[0].command, "ls -la");
        assert!(r2.tool_calls[0].is_read_only);
        assert_eq!(r2.tool_calls[1].command, "cat README.md");

        // Chunk 3: trailing prose with no markers.
        let r3 = filter.ingest("Now I'll summarize what I found.");
        assert_eq!(
            r3.text_chunks,
            vec!["Now I'll summarize what I found.".to_string()]
        );

        // Flush — nothing remains buffered after normal prose was streamed.
        let flushed = filter.flush();
        assert!(flushed.text_chunks.is_empty());
        assert!(flushed.tool_calls.is_empty());
    }

    #[test]
    fn streaming_filter_emits_prose_without_waiting_for_newline() {
        let mut filter = StreamingToolCallFilter::default();

        let result = filter.ingest("Codex is still working");

        assert_eq!(result.text_chunks, vec!["Codex is still working"]);
        assert!(result.tool_calls.is_empty());
        assert!(filter.flush().text_chunks.is_empty());
    }

    #[test]
    fn local_agent_stream_error_preserves_existing_text() {
        assert_eq!(
            local_agent_stream_error_text("Partial answer", "Local Agent timed out"),
            "Partial answer\n\nLocal Agent timed out"
        );
        assert_eq!(
            local_agent_stream_error_text("", "Local Agent timed out"),
            "Local Agent timed out"
        );
    }

    #[test]
    fn streaming_filter_drops_partial_marker_on_flush() {
        // The CLI exits mid-marker (network drop, kill, etc.). Don't expose a
        // half-baked `DWARF_TOOL_CALL {"type` fragment to the user.
        let mut filter = StreamingToolCallFilter::default();
        filter.ingest("Some prose.\nDWARF_TOOL_CALL {\"type\":\"run_sh");
        let flushed = filter.flush();
        assert!(flushed.text_chunks.is_empty());
        assert!(flushed.tool_calls.is_empty());
    }

    #[test]
    fn streaming_filter_handles_multiple_lines_in_one_chunk() {
        let mut filter = StreamingToolCallFilter::default();
        let r = filter.ingest(
            "First line.\nSecond line.\nDWARF_TOOL_CALL {\"type\":\"run_shell_command\",\"command\":\"pwd\"}\nTrailing.",
        );
        assert_eq!(
            r.text_chunks,
            vec![
                "First line.\n".to_string(),
                "Second line.\n".to_string(),
                "Trailing.".to_string()
            ]
        );
        assert_eq!(r.tool_calls.len(), 1);
        assert_eq!(r.tool_calls[0].command, "pwd");

        let flushed = filter.flush();
        assert!(flushed.text_chunks.is_empty());
    }

    #[test]
    fn extract_dwarf_tool_calls_passes_through_when_no_marker() {
        // Fast path: no TOOL_CALL_PREFIX anywhere → return verbatim, no extraction.
        let input = "Some response text\nwith multiple lines";
        let (cleaned, calls) = extract_dwarf_tool_calls(input);
        assert_eq!(cleaned, input);
        assert!(calls.is_empty());
    }

    #[test]
    fn extract_dwarf_tool_calls_splits_marker_from_prose() {
        let input = "I'll check that for you.\n\
            DWARF_TOOL_CALL: {\"type\":\"run_shell_command\",\"command\":\"ls\",\"is_read_only\":true}\n\
            Done.";
        let (cleaned, calls) = extract_dwarf_tool_calls(input);
        assert_eq!(cleaned, "I'll check that for you.\nDone.");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].command, "ls");
        assert!(calls[0].is_read_only);
    }

    #[test]
    fn stderr_cap_is_enforced() {
        let mut buf = String::new();
        // Push enough lines to overrun the cap; assert we don't grow without bound.
        for i in 0..1_000 {
            append_stderr(&mut buf, &format!("error line {i:04}: blah blah blah"));
        }
        assert!(buf.len() <= STDERR_CAP_BYTES + 64); // small overshoot from final line
    }

    #[test]
    fn finds_root_task_id() {
        let root = api::Task {
            id: "root".to_string(),
            dependencies: None,
            messages: vec![],
            description: String::new(),
            summary: String::new(),
            server_data: String::new(),
        };
        let child = api::Task {
            id: "child".to_string(),
            dependencies: Some(api::task::Dependencies {
                parent_task_id: "root".to_string(),
            }),
            messages: vec![],
            description: String::new(),
            summary: String::new(),
            server_data: String::new(),
        };

        assert_eq!(root_task_id(&[child, root]), Some("root".to_string()));
    }

    #[test]
    fn env_model_overrides_request_model() {
        assert_eq!(
            selected_codex_model(Some("gpt-5.4".to_string()), "gpt-5.5"),
            Some("gpt-5.4".to_string())
        );
    }

    #[test]
    fn ignores_non_codex_request_model() {
        assert_eq!(selected_codex_model(None, "auto"), None);
        assert_eq!(selected_codex_model(None, "claude-sonnet"), None);
    }

    #[test]
    fn selects_claude_runtime_from_model() {
        assert_eq!(
            local_agent_runtime_for_model("claude-code"),
            LocalAgentRuntime::Claude
        );
        assert_eq!(
            local_agent_runtime_for_model("sonnet"),
            LocalAgentRuntime::Claude
        );
        assert_eq!(
            local_agent_runtime_for_model("gpt-5.5"),
            LocalAgentRuntime::Codex
        );
    }

    #[test]
    fn routes_default_codex_model_to_claude_when_only_claude_is_authed() {
        assert_eq!(
            runtime_for_model(
                "gpt-5.5",
                LocalAuthState {
                    codex: false,
                    claude: true,
                }
            ),
            LocalAgentRuntime::Claude
        );
    }

    #[test]
    fn routes_explicit_codex_model_to_codex_when_only_claude_is_authed() {
        assert_eq!(
            runtime_for_model(
                "gpt-5.4",
                LocalAuthState {
                    codex: false,
                    claude: true,
                }
            ),
            LocalAgentRuntime::Codex
        );
    }

    #[test]
    fn routes_default_claude_model_to_codex_when_only_codex_is_authed() {
        assert_eq!(
            runtime_for_model(
                "claude-code",
                LocalAuthState {
                    codex: true,
                    claude: false,
                }
            ),
            LocalAgentRuntime::Codex
        );
    }

    #[test]
    fn routes_explicit_claude_model_to_claude_when_only_codex_is_authed() {
        assert_eq!(
            runtime_for_model(
                "opus",
                LocalAuthState {
                    codex: true,
                    claude: false,
                }
            ),
            LocalAgentRuntime::Claude
        );
    }

    #[test]
    fn selects_claude_model_from_request_model() {
        assert_eq!(selected_claude_model(None, "claude-code"), None);
        assert_eq!(
            selected_claude_model(None, "sonnet"),
            Some("sonnet".to_string())
        );
        assert_eq!(
            selected_claude_model(Some("opus".to_string()), "sonnet"),
            Some("opus".to_string())
        );
    }

    #[test]
    fn extracts_claude_agent_text_from_json() {
        let stdout = r#"{"type":"result","result":"hello from claude"}"#;

        assert_eq!(
            extract_claude_agent_text(stdout).as_deref(),
            Some("hello from claude")
        );
    }

    #[test]
    fn detects_cwd_change_target_from_codex_output() {
        let target_dir = std::env::temp_dir().join(format!(
            "{}-local-codex-{}",
            ChannelState::app_name(),
            Uuid::new_v4()
        ));
        std::fs::create_dir(&target_dir).unwrap();
        let output = format!(
            "Found it and moved context to:\n\n{}\n",
            target_dir.display()
        );
        let inputs = vec![user_query("find fileloom and move to the directory")];

        assert_eq!(
            local_cwd_change_target(&inputs, &output, None).as_deref(),
            Some(target_dir.as_path())
        );

        std::fs::remove_dir(target_dir).unwrap();
    }

    #[test]
    fn detects_move_me_to_directory_request() {
        let target_dir = std::env::temp_dir().join(format!(
            "{}-local-codex-{}",
            ChannelState::app_name(),
            Uuid::new_v4()
        ));
        std::fs::create_dir(&target_dir).unwrap();
        let output = format!(
            "Now using {} for subsequent commands.\n\nValidated with pwd: {}.",
            target_dir.display(),
            target_dir.display()
        );
        let inputs = vec![user_query(&format!("move me to {}", target_dir.display()))];

        assert_eq!(
            local_cwd_change_target(&inputs, &output, None).as_deref(),
            Some(target_dir.as_path())
        );

        std::fs::remove_dir(target_dir).unwrap();
    }

    #[test]
    fn detects_directory_target_from_user_prompt() {
        let target_dir = std::env::temp_dir().join(format!(
            "{}-local-codex-{}",
            ChannelState::app_name(),
            Uuid::new_v4()
        ));
        std::fs::create_dir(&target_dir).unwrap();
        let inputs = vec![user_query(&format!("move me to {}", target_dir.display()))];

        assert_eq!(
            local_cwd_change_target(&inputs, "Done.", None).as_deref(),
            Some(target_dir.as_path())
        );

        std::fs::remove_dir(target_dir).unwrap();
    }

    #[test]
    fn detects_bring_me_into_directory_request() {
        let target_dir = std::env::temp_dir().join(format!(
            "{}-local-codex-{}",
            ChannelState::app_name(),
            Uuid::new_v4()
        ));
        std::fs::create_dir(&target_dir).unwrap();
        let inputs = vec![user_query(&format!(
            "bring me into {}",
            target_dir.display()
        ))];

        assert_eq!(
            local_cwd_change_target(&inputs, "Done.", None).as_deref(),
            Some(target_dir.as_path())
        );

        std::fs::remove_dir(target_dir).unwrap();
    }

    #[test]
    fn detects_find_and_bring_me_there_target() {
        assert_eq!(
            find_and_cd_search_target("find where the fileloom is and bring me there.").as_deref(),
            Some("fileloom")
        );
        assert_eq!(
            find_and_cd_search_target("find Fileloom and bring me there").as_deref(),
            Some("Fileloom")
        );
    }

    #[test]
    fn creates_direct_find_and_cd_tool_call() {
        let inputs = vec![user_query("find where the fileloom is and bring me there.")];

        let tool_call = direct_terminal_tool_call(&inputs, None).unwrap();

        assert!(tool_call.command.contains("mdfind"));
        assert!(tool_call.command.contains("find \"$HOME/Documents\""));
        assert!(tool_call.command.contains("cd \"$target\" && pwd"));
        assert!(tool_call.is_read_only);
    }

    #[test]
    fn creates_direct_cd_tool_call_for_existing_prompt_path() {
        let target_dir = std::env::temp_dir().join(format!(
            "{}-local-codex-{}",
            ChannelState::app_name(),
            Uuid::new_v4()
        ));
        std::fs::create_dir(&target_dir).unwrap();
        let inputs = vec![user_query(&format!(
            "bring me into {}",
            target_dir.display()
        ))];

        let tool_call = direct_terminal_tool_call(&inputs, None).unwrap();

        assert_eq!(
            tool_call.command,
            format!("cd {}", shell_quote_path(&target_dir))
        );

        std::fs::remove_dir(target_dir).unwrap();
    }

    #[test]
    fn ignores_directory_paths_without_cwd_change_intent() {
        let target_dir = std::env::temp_dir();
        let output = format!("The directory is {}.", target_dir.display());
        let inputs = vec![user_query("where is the temp dir?")];

        assert_eq!(local_cwd_change_target(&inputs, &output, None), None);
    }

    #[test]
    fn extracts_dwarf_tool_call_markers_from_output() {
        let output = "I'll inspect it.\nDWARF_TOOL_CALL {\"type\":\"run_shell_command\",\"command\":\"rg foo\",\"is_read_only\":true,\"uses_pager\":false,\"is_risky\":false,\"wait_until_complete\":true}\nThen I'll use the result.";

        let (cleaned, tool_calls) = extract_dwarf_tool_calls(output);

        assert_eq!(cleaned, "I'll inspect it.\nThen I'll use the result.");
        assert_eq!(
            tool_calls,
            vec![LocalRunShellCommand {
                command: "rg foo".to_string(),
                is_read_only: true,
                uses_pager: false,
                is_risky: false,
                wait_until_complete: true,
            }]
        );
    }

    #[test]
    fn extracts_colon_prefixed_dwarf_tool_call_markers() {
        let output = "DWARF_TOOL_CALL: {\"type\":\"run_shell_command\",\"command\":\"cargo test\"}";

        let (cleaned, tool_calls) = extract_dwarf_tool_calls(output);

        assert_eq!(
            cleaned,
            format!(
                "I'll run the requested command in {}.",
                ChannelState::app_name_display()
            )
        );
        assert_eq!(
            tool_calls,
            vec![LocalRunShellCommand {
                command: "cargo test".to_string(),
                is_read_only: true,
                uses_pager: false,
                is_risky: false,
                wait_until_complete: true,
            }]
        );
    }

    #[test]
    fn extracts_local_next_command_json() {
        let output = "```json\n{\"command\":\"cargo test -p warp local_codex\"}\n```";

        assert_eq!(
            extract_local_next_command(output).as_deref(),
            Some("cargo test -p warp local_codex")
        );
    }

    #[test]
    fn extracts_local_prompt_suggestion_json() {
        let output = "Sure.\n{\"query\":\"Check the failing test output and run the smallest relevant test.\"}";

        assert_eq!(
            extract_local_prompt_suggestion_query(output).as_deref(),
            Some("Check the failing test output and run the smallest relevant test.")
        );
    }

    #[test]
    fn extracts_working_directory_from_context_messages() {
        let temp_dir = std::env::temp_dir();
        let message = format!(
            "{{\"input\":\"pwd\",\"context\":{{\"pwd\":\"{}\"}}}}",
            temp_dir.display()
        );

        assert_eq!(
            working_directory_from_context_messages(&[message]).as_deref(),
            Some(temp_dir.to_string_lossy().as_ref())
        );
    }

    #[test]
    fn quotes_shell_paths() {
        assert_eq!(
            shell_quote_path(Path::new("/tmp/path with spaces")),
            "'/tmp/path with spaces'"
        );
        assert_eq!(
            shell_quote_path(Path::new("/tmp/path with 'quote'")),
            "'/tmp/path with '\\''quote'\\'''"
        );
    }

    #[test]
    fn nvm_binary_candidates_are_newest_first() {
        let home = Path::new("/Users/example");
        let candidates = [
            home.join(".nvm/versions/node/v18.0.0/bin/codex"),
            home.join(".nvm/versions/node/v20.0.0/bin/codex"),
        ];
        let mut sorted = candidates.to_vec();
        sorted.sort_by(|a, b| b.cmp(a));

        assert_eq!(sorted[0], home.join(".nvm/versions/node/v20.0.0/bin/codex"));
    }

    fn user_query(query: &str) -> AIAgentInput {
        AIAgentInput::UserQuery {
            query: query.to_string(),
            context: std::sync::Arc::from([]),
            static_query_type: None,
            referenced_attachments: std::collections::HashMap::new(),
            user_query_mode: crate::ai::agent::UserQueryMode::Normal,
            running_command: None,
            intended_agent: None,
        }
    }
}
