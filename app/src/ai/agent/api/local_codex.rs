use std::{
    env,
    path::{Path, PathBuf},
    process::{Command as StdCommand, Stdio},
};

use ai::{local_claude_auth, local_openai_auth};
use command::r#async::Command;
use futures::Stream;
use futures_lite::{io::BufReader, stream, AsyncBufReadExt, StreamExt as _};
use prost_types::FieldMask;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;
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
    _cancellation_rx: futures::channel::oneshot::Receiver<()>,
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
    // and emit the whole exchange in one shot — there's nothing to stream.
    if let Some(tool_call) = direct_terminal_tool_call(&params.input, working_directory.as_deref())
    {
        let output_text = if tool_call.command.starts_with("target=$(mdfind ") {
            "I'll find the matching directory and switch Dwarf to it.".to_string()
        } else {
            format!("I'll run `{}` in Dwarf.", tool_call.command)
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

    // Streaming path: emit init + (optional) CreateTask + seed AddMessagesToTask with an empty
    // AgentOutput, then drive the CLI and forward text deltas as AppendToMessageContent events.
    let message_id = format!("local-{}-message-{}", runtime.slug(), Uuid::new_v4());
    let inputs = params.input.clone();
    let stream = async_stream::stream! {
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

        let mut seed_messages = user_query_messages(&inputs, &task_id, &request_id);
        seed_messages.push(api::Message {
            id: message_id.clone(),
            task_id: task_id.clone(),
            request_id: request_id.clone(),
            timestamp: None,
            server_message_data: String::new(),
            citations: vec![],
            message: Some(api::message::Message::AgentOutput(
                api::message::AgentOutput { text: String::new() },
            )),
        });
        yield Ok(client_actions_event(vec![api::ClientAction {
            action: Some(api::client_action::Action::AddMessagesToTask(
                api::client_action::AddMessagesToTask {
                    task_id: task_id.clone(),
                    messages: seed_messages,
                },
            )),
        }]));

        let mut full_text = String::new();
        let mut delta_stream: std::pin::Pin<Box<dyn Stream<Item = Result<String, String>> + Send>> =
            match runtime {
                LocalAgentRuntime::Codex => {
                    let model = model_for_codex(&params);
                    let prompt = prompt_for_local_agent(&params, runtime, model.as_deref());
                    Box::pin(run_codex_streaming(
                        prompt,
                        working_directory.clone(),
                        model,
                    ))
                }
                LocalAgentRuntime::Claude => {
                    let model = model_for_claude(&params);
                    let prompt = prompt_for_local_agent(&params, runtime, model.as_deref());
                    Box::pin(run_claude_streaming(
                        prompt,
                        working_directory.clone(),
                        model,
                    ))
                }
            };

        while let Some(item) = delta_stream.next().await {
            match item {
                Ok(chunk) => {
                    if chunk.is_empty() {
                        continue;
                    }
                    full_text.push_str(&chunk);
                    yield Ok(append_agent_text_event(&task_id, &message_id, chunk));
                }
                Err(error) => {
                    full_text = error.clone();
                    yield Ok(replace_agent_text_event(
                        &task_id,
                        &request_id,
                        &message_id,
                        error,
                    ));
                    break;
                }
            }
        }

        // Post-process: pull out DWARF_TOOL_CALL markers and any cwd-change tool call.
        let (mut cleaned_text, mut tool_calls) = extract_dwarf_tool_calls(&full_text);
        if let Some(target_dir) =
            local_cwd_change_target(&inputs, &cleaned_text, working_directory.as_deref())
        {
            let command = format!("cd {}", shell_quote_path(&target_dir));
            if !tool_calls.iter().any(|call| call.command == command) {
                tool_calls.push(LocalRunShellCommand::read_only(command));
            }
            if tool_calls.len() == 1 {
                cleaned_text = format!("I'll run `{}` in Dwarf.", tool_calls[0].command);
            }
        }

        if cleaned_text != full_text {
            yield Ok(replace_agent_text_event(
                &task_id,
                &request_id,
                &message_id,
                cleaned_text,
            ));
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
    format!(
        "You are generating a local Dwarf terminal autosuggestion.\n\
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
    format!(
        "You are generating a local Dwarf agent follow-up chip.\n\
         Return exactly one JSON object and no prose: {{\"query\":\"...\"}}.\n\
         Suggest one concise natural-language instruction for the local Dwarf agent based on the terminal context.\n\
         Prefer instructions that ask Dwarf to inspect, run, test, debug, or summarize with local commands when useful.\n\
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
) -> impl Stream<Item = Result<String, String>> + Send {
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
        let stdout_stream: std::pin::Pin<Box<dyn Stream<Item = MergedLine> + Send>> = Box::pin(
            BufReader::new(stdout)
                .lines()
                .filter_map(|r| r.ok().map(MergedLine::Stdout)),
        );
        let stderr_stream: std::pin::Pin<Box<dyn Stream<Item = MergedLine> + Send>> = Box::pin(
            BufReader::new(stderr)
                .lines()
                .filter_map(|r| r.ok().map(MergedLine::Stderr)),
        );
        let mut merged = futures::stream::select(stdout_stream, stderr_stream);

        let mut messages_in_order: Vec<(String, String)> = Vec::new();
        let mut emitted_total = String::new();
        let mut stderr_buf = String::new();

        while let Some(line) = merged.next().await {
            match line {
                MergedLine::Stdout(line) => {
                    if let Some(delta) =
                        ingest_codex_line(&line, &mut messages_in_order, &mut emitted_total)
                    {
                        if !delta.is_empty() {
                            yield Ok(delta);
                        }
                    }
                }
                MergedLine::Stderr(line) => {
                    if !stderr_buf.is_empty() {
                        stderr_buf.push('\n');
                    }
                    stderr_buf.push_str(&line);
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
            if emitted_total.is_empty() {
                yield Err(format!(
                    "{LOCAL_AGENT_DISPLAY_NAME} finished without an agent message.\n\n```text\n{}\n```",
                    truncate(&stderr_buf, 4_000)
                ));
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
            Ok(chunk) => full.push_str(&chunk),
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
) -> impl Stream<Item = Result<String, String>> + Send {
    async_stream::stream! {
        let claude_bin = claude_bin();
        let mut command = Command::new(claude_bin.clone());
        command
            .arg("-p")
            .arg("--output-format")
            .arg("stream-json")
            .arg("--verbose")
            .arg("--tools")
            .arg("")
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
        let stdout_stream: std::pin::Pin<Box<dyn Stream<Item = MergedLine> + Send>> = Box::pin(
            BufReader::new(stdout)
                .lines()
                .filter_map(|r| r.ok().map(MergedLine::Stdout)),
        );
        let stderr_stream: std::pin::Pin<Box<dyn Stream<Item = MergedLine> + Send>> = Box::pin(
            BufReader::new(stderr)
                .lines()
                .filter_map(|r| r.ok().map(MergedLine::Stderr)),
        );
        let mut merged = futures::stream::select(stdout_stream, stderr_stream);

        let mut assistant_chunks: Vec<String> = Vec::new();
        let mut emitted_total = String::new();
        let mut stderr_buf = String::new();

        while let Some(line) = merged.next().await {
            match line {
                MergedLine::Stdout(line) => {
                    if let Some(delta) = ingest_claude_line(
                        &line,
                        &mut assistant_chunks,
                        &mut emitted_total,
                    ) {
                        if !delta.is_empty() {
                            yield Ok(delta);
                        }
                    }
                }
                MergedLine::Stderr(line) => {
                    if !stderr_buf.is_empty() {
                        stderr_buf.push('\n');
                    }
                    stderr_buf.push_str(&line);
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
            if emitted_total.is_empty() {
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

fn ingest_codex_line(
    line: &str,
    messages_in_order: &mut Vec<(String, String)>,
    emitted_total: &mut String,
) -> Option<String> {
    let value: Value = serde_json::from_str(line).ok()?;
    let item = value.get("item")?;
    if item.get("type")?.as_str()? != "agent_message" {
        return None;
    }
    let id = item.get("id")?.as_str()?.to_string();
    let text = item.get("text")?.as_str()?.to_string();
    if let Some(entry) = messages_in_order.iter_mut().find(|(eid, _)| eid == &id) {
        entry.1 = text;
    } else {
        messages_in_order.push((id, text));
    }
    let running = messages_in_order
        .iter()
        .map(|(_, t)| t.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");
    if running.len() > emitted_total.len() && running.starts_with(&*emitted_total) {
        let delta = running[emitted_total.len()..].to_string();
        *emitted_total = running;
        Some(delta)
    } else {
        None
    }
}

fn ingest_claude_line(
    line: &str,
    assistant_chunks: &mut Vec<String>,
    emitted_total: &mut String,
) -> Option<String> {
    let value: Value = serde_json::from_str(line).ok()?;
    let event_type = value.get("type")?.as_str()?;
    let text: String = match event_type {
        "assistant" => {
            let content = value.get("message")?.get("content")?.as_array()?;
            content
                .iter()
                .filter_map(|c| c.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("")
        }
        "result" if assistant_chunks.is_empty() => value
            .get("result")
            .and_then(Value::as_str)?
            .to_string(),
        _ => return None,
    };
    if text.is_empty() {
        return None;
    }
    assistant_chunks.push(text);
    let running = assistant_chunks.join("\n\n");
    if running.len() > emitted_total.len() && running.starts_with(&*emitted_total) {
        let delta = running[emitted_total.len()..].to_string();
        *emitted_total = running;
        Some(delta)
    } else {
        None
    }
}

fn codex_bin() -> String {
    configured_codex_bin()
        .or_else(find_codex_bin)
        .unwrap_or_else(|| "codex".to_string())
}

fn claude_bin() -> String {
    configured_claude_bin()
        .or_else(find_claude_bin)
        .unwrap_or_else(|| "claude".to_string())
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
    common_codex_candidates()
        .into_iter()
        .find(|path| is_executable(path))
        .map(path_to_string)
}

fn find_common_claude_bin() -> Option<String> {
    common_claude_candidates()
        .into_iter()
        .find(|path| is_executable(path))
        .map(path_to_string)
}

fn common_codex_candidates() -> Vec<PathBuf> {
    let mut candidates = vec![
        PathBuf::from("/opt/homebrew/bin/codex"),
        PathBuf::from("/usr/local/bin/codex"),
    ];

    if let Some(home) = env::var_os("HOME").map(PathBuf::from) {
        candidates.extend([
            home.join(".local/bin/codex"),
            home.join(".volta/bin/codex"),
            home.join(".asdf/shims/codex"),
            home.join(".npm-global/bin/codex"),
            home.join(".bun/bin/codex"),
        ]);
        candidates.extend(nvm_binary_candidates(&home, "codex"));
    }

    candidates
}

fn common_claude_candidates() -> Vec<PathBuf> {
    let mut candidates = vec![
        PathBuf::from("/opt/homebrew/bin/claude"),
        PathBuf::from("/usr/local/bin/claude"),
    ];

    if let Some(home) = env::var_os("HOME").map(PathBuf::from) {
        candidates.extend([
            home.join(".local/bin/claude"),
            home.join(".volta/bin/claude"),
            home.join(".asdf/shims/claude"),
            home.join(".npm-global/bin/claude"),
            home.join(".bun/bin/claude"),
        ]);
        candidates.extend(nvm_binary_candidates(&home, "claude"));
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
    prompt.push_str("Dwarf local agent session context:\n");
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
    prompt.push_str(
        "If the user asks what model you are, report this configured model label and do not claim a separate runtime label you cannot inspect.\n\n",
    );
    prompt.push_str(
        "Dwarf terminal tool-call contract:\n\
         - Dwarf is a terminal. Prefer making progress with shell commands, scripts, repository inspection, tests, and concise analysis over conversational-only answers.\n\
         - You cannot change Dwarf's live terminal by changing your own local agent subprocess working directory.\n\
         - When the user asks to run, inspect, analyze, search, test, build, install, execute a script, or change directories, emit one tool-call marker per required shell command on its own line:\n\
         DWARF_TOOL_CALL {\"type\":\"run_shell_command\",\"command\":\"pwd\",\"is_read_only\":true,\"uses_pager\":false,\"is_risky\":false,\"wait_until_complete\":true}\n\
         - Emit one self-contained command when a later step depends on an earlier command's output. Do not emit dependent multi-step plans because Dwarf will not feed command results back to you automatically in this local bridge.\n\
         - For directory changes, emit `cd <path>` as a Dwarf tool call. Do not say the cwd changed until Dwarf returns the command result.\n\
         - For read-only inspection commands such as pwd, ls, find, rg, git status, cargo test --no-run, use `is_read_only:true`.\n\
         - For scripts, builds, tests that execute project code, or commands that may modify files, set `is_read_only:false`. Set `is_risky:true` only for destructive, credential, network, sudo, or external side-effect commands.\n\
         - Do not wrap DWARF_TOOL_CALL lines in markdown fences. Keep prose short and do not claim validation from commands that only ran in your local agent subprocess.\n\n",
    );

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

fn extract_dwarf_tool_calls(output_text: &str) -> (String, Vec<LocalRunShellCommand>) {
    let mut output_lines = Vec::new();
    let mut tool_calls = Vec::new();

    for line in output_text.lines() {
        let Some(json) = dwarf_tool_call_json(line) else {
            output_lines.push(line);
            continue;
        };

        let Ok(marker) = serde_json::from_str::<DwarfToolCallMarker>(json) else {
            output_lines.push(line);
            continue;
        };
        if marker.kind != "run_shell_command" || marker.command.trim().is_empty() {
            output_lines.push(line);
            continue;
        }

        tool_calls.push(LocalRunShellCommand {
            command: marker.command.trim().to_string(),
            is_read_only: marker.is_read_only.unwrap_or(true),
            uses_pager: marker.uses_pager.unwrap_or(false),
            is_risky: marker.is_risky.unwrap_or(false),
            wait_until_complete: marker.wait_until_complete.unwrap_or(true),
        });
    }

    let output_text = output_lines.join("\n").trim().to_string();
    let output_text = if output_text.is_empty() && !tool_calls.is_empty() {
        "I'll run the requested command in Dwarf.".to_string()
    } else {
        output_text
    };

    (output_text, tool_calls)
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

fn append_agent_text_event(
    task_id: &str,
    message_id: &str,
    text: String,
) -> api::ResponseEvent {
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
                    paths: vec!["message.agent_output.text".to_string()],
                }),
            },
        )),
    }])
}

fn replace_agent_text_event(
    task_id: &str,
    request_id: &str,
    message_id: &str,
    text: String,
) -> api::ResponseEvent {
    client_actions_event(vec![api::ClientAction {
        action: Some(api::client_action::Action::UpdateTaskMessage(
            api::client_action::UpdateTaskMessage {
                task_id: task_id.to_string(),
                message: Some(api::Message {
                    id: message_id.to_string(),
                    task_id: task_id.to_string(),
                    request_id: request_id.to_string(),
                    timestamp: None,
                    server_message_data: String::new(),
                    citations: vec![],
                    message: Some(api::message::Message::AgentOutput(
                        api::message::AgentOutput { text },
                    )),
                }),
                mask: Some(FieldMask {
                    paths: vec!["message.agent_output.text".to_string()],
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

    #[test]
    fn ingests_codex_agent_message_jsonl_deltas() {
        let mut messages: Vec<(String, String)> = Vec::new();
        let mut emitted = String::new();
        assert_eq!(
            ingest_codex_line(
                r#"{"type":"thread.started","thread_id":"t"}"#,
                &mut messages,
                &mut emitted,
            ),
            None
        );
        assert_eq!(
            ingest_codex_line(
                r#"{"type":"item.completed","item":{"id":"item_0","type":"agent_message","text":"hel"}}"#,
                &mut messages,
                &mut emitted,
            ),
            Some("hel".to_string())
        );
        assert_eq!(
            ingest_codex_line(
                r#"{"type":"item.updated","item":{"id":"item_0","type":"agent_message","text":"hello"}}"#,
                &mut messages,
                &mut emitted,
            ),
            Some("lo".to_string())
        );
        assert_eq!(
            ingest_codex_line(
                r#"{"type":"item.completed","item":{"id":"item_1","type":"agent_message","text":"world"}}"#,
                &mut messages,
                &mut emitted,
            ),
            Some("\n\nworld".to_string())
        );
        assert_eq!(emitted, "hello\n\nworld");
    }

    #[test]
    fn ingests_claude_stream_json_deltas() {
        let mut chunks: Vec<String> = Vec::new();
        let mut emitted = String::new();
        assert_eq!(
            ingest_claude_line(
                r#"{"type":"system","subtype":"init"}"#,
                &mut chunks,
                &mut emitted,
            ),
            None
        );
        assert_eq!(
            ingest_claude_line(
                r#"{"type":"assistant","message":{"content":[{"type":"text","text":"hello"}]}}"#,
                &mut chunks,
                &mut emitted,
            ),
            Some("hello".to_string())
        );
        assert_eq!(
            ingest_claude_line(
                r#"{"type":"assistant","message":{"content":[{"type":"text","text":"world"}]}}"#,
                &mut chunks,
                &mut emitted,
            ),
            Some("\n\nworld".to_string())
        );
        assert_eq!(emitted, "hello\n\nworld");
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
        let target_dir = std::env::temp_dir().join(format!("dwarf-local-codex-{}", Uuid::new_v4()));
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
        let target_dir = std::env::temp_dir().join(format!("dwarf-local-codex-{}", Uuid::new_v4()));
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
        let target_dir = std::env::temp_dir().join(format!("dwarf-local-codex-{}", Uuid::new_v4()));
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
        let target_dir = std::env::temp_dir().join(format!("dwarf-local-codex-{}", Uuid::new_v4()));
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
        let target_dir = std::env::temp_dir().join(format!("dwarf-local-codex-{}", Uuid::new_v4()));
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

        assert_eq!(cleaned, "I'll run the requested command in Dwarf.");
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
