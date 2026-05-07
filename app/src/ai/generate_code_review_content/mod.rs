// TODO(edward): follow-up — gate callers of this module on both
//   1. an `AISettings` opt-out (mirror `is_shared_block_title_generation_enabled`), and
//   2. a customer-type guard (exclude Enterprise unless Warp plan / dogfood),
// matching the pattern in `terminal/share_block_modal.rs::should_send_title_gen_request`.
// `FeatureFlag::GitOperationsInCodeReview` already gates the surrounding UI,
// but does not address AI-specific privacy / opt-out concerns for sending
// diffs to an LLM.
pub(crate) mod api;

use api::{GenerateCodeReviewContentRequest, GenerateCodeReviewContentResponse, OutputType};

use crate::ai::local_llm::{build_one_shot, LocalLLMError, Provider};

const SYSTEM_PROMPT: &str =
    "You generate concise, well-written content for git commits and pull requests. \
     Reply with the requested content only — no preamble, no markdown fences, no commentary.";

/// Generates code-review content (commit message / PR title / PR description)
/// through a locally-installed CLI agent.
///
/// Replaces the previous hosted-server endpoint
/// (`POST /ai/generate_code_review_content`) so dwarf can produce this content
/// without depending on Warp's backend.
pub async fn generate_locally(
    req: GenerateCodeReviewContentRequest,
) -> Result<GenerateCodeReviewContentResponse, LocalLLMError> {
    // TODO(phase-4): read the user's preferred provider from AISettings.
    let preference = Provider::Codex;
    let one_shot = build_one_shot(preference).ok_or(LocalLLMError::CliNotInstalled {
        binary: preference.binary_name(),
    })?;

    let prompt = build_prompt(&req);
    let content = one_shot.complete(&prompt, Some(SYSTEM_PROMPT)).await?;

    Ok(GenerateCodeReviewContentResponse { content })
}

fn build_prompt(req: &GenerateCodeReviewContentRequest) -> String {
    let task = match req.output_type {
        OutputType::CommitMessage => {
            "Write a single conventional-commit-style commit message for the diff below. \
             Use the format `<type>(<scope>): <subject>` on the first line, optional body \
             paragraphs after a blank line. Subject under 72 characters."
        }
        OutputType::PrTitle => {
            "Write a single concise pull-request title for the diff below. \
             No trailing period. Under 70 characters."
        }
        OutputType::PrDescription => {
            "Write a pull-request description in markdown for the diff below. \
             Include a short '## Summary' bullet list and a '## Test plan' bullet list. \
             Do not include the title."
        }
    };

    let mut sections = vec![task.to_string()];

    if !req.branch_name.is_empty() {
        sections.push(format!("Branch: {}", req.branch_name));
    }

    if !req.commit_messages.is_empty() {
        sections.push(format!(
            "Existing commit messages on this branch:\n{}",
            req.commit_messages
                .iter()
                .map(|m| format!("- {m}"))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }

    sections.push(format!("Diff:\n```\n{}\n```", req.diff));

    sections.join("\n\n")
}
