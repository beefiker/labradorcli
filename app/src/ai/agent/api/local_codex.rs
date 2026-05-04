use std::{
    env,
    path::{Path, PathBuf},
    process::Command as StdCommand,
};

use command::r#async::Command;
use futures_lite::stream;
use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;
use warp_multi_agent_api as api;

use crate::ai::agent::AIAgentInput;

use super::{ConvertToAPITypeError, RequestParams, ResponseStream};

const CODEX_BIN_ENV_VAR: &str = "DWARF_CODEX_BIN";
const CODEX_MODEL_ENV_VAR: &str = "DWARF_CODEX_MODEL";
const TOOL_CALL_PREFIX: &str = "DWARF_TOOL_CALL";

pub(super) async fn generate_output(
    params: RequestParams,
    _cancellation_rx: futures::channel::oneshot::Receiver<()>,
) -> Result<ResponseStream, ConvertToAPITypeError> {
    let request_id = format!("local-codex-request-{}", Uuid::new_v4());
    let conversation_id = params
        .conversation_token
        .as_ref()
        .map(|token| token.as_str().to_string())
        .unwrap_or_else(|| format!("local-codex-conversation-{}", Uuid::new_v4()));

    let root_task_id = root_task_id(&params.tasks);
    let needs_create_task = root_task_id.is_none();
    let task_id = root_task_id.unwrap_or_else(|| format!("local-codex-task-{}", Uuid::new_v4()));
    let working_directory = params
        .session_context
        .current_working_directory()
        .as_deref()
        .map(str::to_string);

    let direct_tool_call = direct_terminal_tool_call(&params.input, working_directory.as_deref());
    let output_text = if let Some(tool_call) = direct_tool_call.as_ref() {
        if tool_call.command.starts_with("target=$(mdfind ") {
            "I'll find the matching directory and switch Dwarf to it.".to_string()
        } else {
            format!("I'll run `{}` in Dwarf.", tool_call.command)
        }
    } else {
        let model = model_for_codex(&params);
        let prompt = prompt_for_codex(&params, model.as_deref());
        let codex_output = run_codex(prompt, working_directory.as_deref(), model.as_deref()).await;
        match codex_output {
            Ok(text) => text,
            Err(error) => error,
        }
    };

    let mut events = vec![Ok(init_event(conversation_id, request_id.clone()))];
    if needs_create_task {
        events.push(Ok(client_actions_event(vec![api::ClientAction {
            action: Some(api::client_action::Action::CreateTask(
                api::client_action::CreateTask {
                    task: Some(api::Task {
                        id: task_id.clone(),
                        description: "Local Codex".to_string(),
                        dependencies: None,
                        messages: vec![],
                        summary: String::new(),
                        server_data: String::new(),
                    }),
                },
            )),
        }])));
    }

    let (mut output_text, mut tool_calls) = if let Some(tool_call) = direct_tool_call {
        (output_text, vec![tool_call])
    } else {
        extract_dwarf_tool_calls(&output_text)
    };
    if let Some(target_dir) =
        local_cwd_change_target(&params.input, &output_text, working_directory.as_deref())
    {
        let command = format!("cd {}", shell_quote_path(&target_dir));
        if !tool_calls.iter().any(|call| call.command == command) {
            tool_calls.push(LocalRunShellCommand::read_only(command));
        }
        if tool_calls.len() == 1 {
            output_text = format!("I'll run `{}` in Dwarf.", tool_calls[0].command);
        }
    }

    let mut messages = user_query_messages(&params.input, &task_id, &request_id);
    messages.push(api::Message {
        id: format!("local-codex-message-{}", Uuid::new_v4()),
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

    Ok(Box::pin(stream::iter(events)))
}

async fn run_codex(
    prompt: String,
    working_directory: Option<&str>,
    model: Option<&str>,
) -> Result<String, String> {
    let codex_bin = codex_bin();
    let mut command = Command::new(codex_bin.clone());
    command
        .arg("exec")
        .arg("--json")
        .arg("--skip-git-repo-check")
        .arg("--sandbox")
        .arg("workspace-write");
    if let Some(model) = model {
        command.arg("--model").arg(model);
    }
    if let Some(working_directory) = working_directory {
        command.arg("-C").arg(working_directory);
    }
    command.arg(prompt);

    let output = command.output().await.map_err(|error| {
        format!(
            "Local Codex failed to start. Tried `{codex_bin}`. Set {CODEX_BIN_ENV_VAR} to your Codex CLI path if it is not on PATH.\n\n```text\n{error}\n```"
        )
    })?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if output.status.success() {
        extract_codex_agent_text(&stdout).ok_or_else(|| {
            format!(
                "Local Codex finished without an agent message.\n\n```text\n{}\n```",
                truncate(&stdout, 4_000)
            )
        })
    } else {
        Err(format!(
            "Local Codex exited with status {}.\n\n```text\n{}\n```",
            output.status,
            truncate(&stderr, 4_000)
        ))
    }
}

fn codex_bin() -> String {
    configured_codex_bin()
        .or_else(find_codex_bin)
        .unwrap_or_else(|| "codex".to_string())
}

fn configured_codex_bin() -> Option<String> {
    env::var(CODEX_BIN_ENV_VAR)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn find_codex_bin() -> Option<String> {
    find_executable_in_path("codex")
        .or_else(find_common_codex_bin)
        .or_else(find_codex_with_user_shell)
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
        candidates.extend(nvm_codex_candidates(&home));
    }

    candidates
}

fn nvm_codex_candidates(home: &Path) -> Vec<PathBuf> {
    let node_versions_dir = home.join(".nvm/versions/node");
    let Ok(entries) = std::fs::read_dir(node_versions_dir) else {
        return Vec::new();
    };

    let mut candidates = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path().join("bin/codex"))
        .collect::<Vec<_>>();
    candidates.sort_by(|a, b| b.cmp(a));
    candidates
}

fn find_codex_with_user_shell() -> Option<String> {
    let shell = env::var_os("SHELL").unwrap_or_else(|| "/bin/zsh".into());
    let output = StdCommand::new(shell)
        .arg("-lc")
        .arg("command -v codex")
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

fn path_to_string(path: PathBuf) -> String {
    path.to_string_lossy().into_owned()
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

fn prompt_for_codex(params: &RequestParams, model: Option<&str>) -> String {
    let mut prompt = String::new();
    prompt.push_str("Dwarf local Codex session context:\n");
    match model {
        Some(model) => {
            prompt.push_str("The Codex CLI was invoked with configured model label `");
            prompt.push_str(model);
            prompt.push_str("`.\n");
        }
        None => {
            prompt.push_str(
                "The Codex CLI was invoked without an explicit `--model` flag, so it is using the Codex CLI default model.\n",
            );
        }
    }
    prompt.push_str(
        "If the user asks what model you are, report this configured model label and do not claim a separate runtime label you cannot inspect.\n\n",
    );
    prompt.push_str(
        "Dwarf terminal tool-call contract:\n\
         - Dwarf is a terminal. Prefer making progress with shell commands, scripts, repository inspection, tests, and concise analysis over conversational-only answers.\n\
         - You cannot change Dwarf's live terminal by changing your own Codex subprocess working directory.\n\
         - When the user asks to run, inspect, analyze, search, test, build, install, execute a script, or change directories, emit one tool-call marker per required shell command on its own line:\n\
         DWARF_TOOL_CALL {\"type\":\"run_shell_command\",\"command\":\"pwd\",\"is_read_only\":true,\"uses_pager\":false,\"is_risky\":false,\"wait_until_complete\":true}\n\
         - Emit one self-contained command when a later step depends on an earlier command's output. Do not emit dependent multi-step plans because Dwarf will not feed command results back to you automatically in this local bridge.\n\
         - For directory changes, emit `cd <path>` as a Dwarf tool call. Do not say the cwd changed until Dwarf returns the command result.\n\
         - For read-only inspection commands such as pwd, ls, find, rg, git status, cargo test --no-run, use `is_read_only:true`.\n\
         - For scripts, builds, tests that execute project code, or commands that may modify files, set `is_read_only:false`. Set `is_risky:true` only for destructive, credential, network, sudo, or external side-effect commands.\n\
         - Do not wrap DWARF_TOOL_CALL lines in markdown fences. Keep prose short and do not claim validation from commands that only ran in your Codex subprocess.\n\n",
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

fn extract_codex_agent_text(stdout: &str) -> Option<String> {
    let messages = stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter_map(|value| {
            let item = value.get("item")?;
            (value.get("type")?.as_str()? == "item.completed"
                && item.get("type")?.as_str()? == "agent_message")
                .then(|| item.get("text")?.as_str().map(str::to_string))?
        })
        .collect::<Vec<_>>();
    (!messages.is_empty()).then(|| messages.join("\n\n"))
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
    fn extracts_codex_agent_text_from_jsonl() {
        let stdout = r#"{"type":"thread.started","thread_id":"t"}
{"type":"item.completed","item":{"id":"item_0","type":"agent_message","text":"hello"}}"#;

        assert_eq!(extract_codex_agent_text(stdout).as_deref(), Some("hello"));
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
    fn nvm_codex_candidates_are_newest_first() {
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
