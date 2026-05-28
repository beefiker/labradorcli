//! One-shot prompt-suggestion generation through a local CLI agent.
//!
//! Replaces the MAA streaming `SuggestPrompt` tool call with a focused
//! local-LLM round-trip that returns a single follow-up prompt suggestion
//! (and optional button label) for a given trigger. Code-diff suggestions
//! are NOT generated here — that path is punted until a future phase.

use super::{build_one_shot, LocalLLMError, Provider};
use crate::ai::agent::PassiveSuggestionTrigger;

const SYSTEM_PROMPT: &str =
    "You suggest a single short follow-up prompt the user might want to send to a coding agent, \
     based on what just happened in their terminal/agent session. \
     Reply in EXACTLY this format and nothing else:\n\
     LABEL: <button label, max 8 words>\n\
     PROMPT: <the prompt to send to the agent>\n\
     If you don't have a good suggestion, reply with the single word `NONE`.";

/// Result of a successful prompt suggestion.
pub struct LocalPromptSuggestion {
    pub prompt: String,
    pub label: Option<String>,
}

/// Generate a follow-up prompt suggestion using the user's preferred local
/// CLI agent. Returns `Ok(None)` if the model declined to suggest one.
pub async fn generate_prompt_suggestion(
    trigger: &PassiveSuggestionTrigger,
    conversation_history_markdown: &str,
    default_preference: Provider,
) -> Result<Option<LocalPromptSuggestion>, LocalLLMError> {
    let one_shot = build_one_shot(default_preference).ok_or(LocalLLMError::CliNotInstalled {
        binary: default_preference.binary_name(),
    })?;

    let prompt = build_user_prompt(trigger, conversation_history_markdown);
    let response = one_shot.complete(&prompt, Some(SYSTEM_PROMPT)).await?;

    Ok(parse_response(&response))
}

fn build_user_prompt(trigger: &PassiveSuggestionTrigger, history: &str) -> String {
    let trigger_section = trigger_template(trigger);

    let mut sections = Vec::with_capacity(3);
    sections.push(trigger_section);
    if !history.trim().is_empty() {
        sections.push(format!("Full conversation history:\n{history}"));
    }
    sections.join("\n\n")
}

/// Trigger-specific prompt templates. Each one frames the suggestion task
/// for the LLM — what just happened, and what kind of follow-up to suggest.
fn trigger_template(trigger: &PassiveSuggestionTrigger) -> String {
    match trigger {
        PassiveSuggestionTrigger::FilesChanged => {
            "Files in the workspace just changed. Suggest a follow-up prompt that would be \
             useful right now — for example, asking the agent to write or update unit tests \
             for the changes."
                .to_string()
        }
        PassiveSuggestionTrigger::CommandRun => {
            "A shell command just ran in the terminal. Suggest a follow-up prompt that would \
             be useful — for example, asking the agent to write tests or investigate the \
             output."
                .to_string()
        }
        PassiveSuggestionTrigger::ShellCommandCompleted(t) => {
            let mut text = String::from(
                "A shell command just finished running. Based on what was executed and its \
                 output, suggest a follow-up prompt the user might want to send.\n",
            );
            text.push_str(&format!("Executed command:\n{:?}\n", t.executed_shell_command));
            if !t.relevant_files.is_empty() {
                text.push_str("Files that may be relevant:\n");
                for f in &t.relevant_files {
                    text.push_str(&format!("- {f:?}\n"));
                }
            }
            text
        }
        PassiveSuggestionTrigger::AgentResponseCompleted { exchange_id } => {
            format!(
                "The agent just completed a response (exchange {:?}). Suggest a natural \
                 follow-up prompt the user might want to send next based on the conversation \
                 above.",
                exchange_id
            )
        }
    }
}

fn parse_response(response: &str) -> Option<LocalPromptSuggestion> {
    let trimmed = response.trim();
    if trimmed.eq_ignore_ascii_case("NONE") || trimmed.is_empty() {
        return None;
    }

    let mut label: Option<String> = None;
    let mut prompt: Option<String> = None;
    for line in trimmed.lines() {
        if let Some(rest) = line.strip_prefix("LABEL:").or_else(|| line.strip_prefix("Label:")) {
            let v = rest.trim();
            if !v.is_empty() {
                label = Some(v.to_string());
            }
        } else if let Some(rest) =
            line.strip_prefix("PROMPT:").or_else(|| line.strip_prefix("Prompt:"))
        {
            let v = rest.trim();
            if !v.is_empty() {
                prompt = Some(v.to_string());
            }
        } else if let Some(p) = prompt.as_mut() {
            // Continuation lines for a multi-line prompt.
            p.push('\n');
            p.push_str(line);
        }
    }

    prompt
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .map(|prompt| LocalPromptSuggestion { prompt, label })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_label_and_prompt() {
        let r = parse_response("LABEL: Write tests\nPROMPT: Write unit tests for foo()").unwrap();
        assert_eq!(r.label.as_deref(), Some("Write tests"));
        assert_eq!(r.prompt, "Write unit tests for foo()");
    }

    #[test]
    fn parses_prompt_only() {
        let r = parse_response("PROMPT: Investigate the failure").unwrap();
        assert_eq!(r.label, None);
        assert_eq!(r.prompt, "Investigate the failure");
    }

    #[test]
    fn parses_multiline_prompt() {
        let r = parse_response("LABEL: x\nPROMPT: line1\nline2\nline3").unwrap();
        assert_eq!(r.prompt, "line1\nline2\nline3");
    }

    #[test]
    fn returns_none_for_none_marker() {
        assert!(parse_response("NONE").is_none());
        assert!(parse_response("none").is_none());
    }

    #[test]
    fn returns_none_when_no_prompt() {
        assert!(parse_response("LABEL: x\n(no prompt line)").is_none());
    }
}
