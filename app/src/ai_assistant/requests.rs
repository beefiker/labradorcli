// TODO(roland): Delete all of this once agent mode fully replaces the AI assistant panel.
use std::sync::Arc;

use chrono::Utc;
use futures::stream::AbortHandle;

use warpui::{Entity, ModelContext};

use crate::{
    ai_assistant::utils::{AssistantTranscriptPart, TranscriptPartSubType},
    send_telemetry_from_ctx,
    server::{
        server_api::{ai::AIClient, ServerApi},
        telemetry::{TelemetryEvent, WarpAIRequestResult},
    },
};

use super::{
    execution_context::WarpAiExecutionContext,
    utils::{markdown_segments_from_text, FormattedTranscriptMessage, TranscriptPart},
};

/// The key for the corresponding entry in UserDefaults.
/// Retained as a constant so that any persisted entries from older builds can be
/// located for deletion; nothing in this module reads or writes it now that
/// hosted request-limit tracking has been removed.
pub const REQUEST_LIMIT_INFO_CACHE_KEY: &str = "AIAssistantRequestLimitInfo";

/// Tracks the current request status for making Dwarf AI requests against server.
pub enum RequestStatus {
    /// There isn't a request in flight right now.
    NotInFlight,

    /// There's currently a request in flight.
    InFlight {
        /// The request itself (i.e. the prompt).
        request: FormattedTranscriptMessage,
        /// A handle to abort the request if desired.
        abort_handle: AbortHandle,
    },
}

#[derive(Debug, Clone)]
pub enum GenerateDialogueResult {
    Success {
        answer: String,
        truncated: bool,
        transcript_summarized: bool,
    },
    Failure,
}

pub struct Requests {
    server_api: Arc<ServerApi>,
    request_status: RequestStatus,

    /// The currently displayed transcript.
    current_transcript: Vec<TranscriptPart>,

    /// Has the server summarized the current transcript because it's running long?
    current_transcript_summarized: bool,

    /// When a user Restarts their transcript, we still remember
    /// the previous transcript parts for things like suggestions.
    /// This list is mutually exclusive from current_transcript.
    old_transcript_parts: Vec<TranscriptPart>,

    ai_execution_context: Option<WarpAiExecutionContext>,
}

impl Entity for Requests {
    type Event = Event;
}

pub enum Event {
    RequestFinished { succeeded: bool },
}

/// Public interface.
impl Requests {
    pub fn new(
        server_api: Arc<ServerApi>,
        _ai_client: Arc<dyn AIClient>,
        _ctx: &mut ModelContext<Self>,
    ) -> Self {
        Self {
            server_api,
            current_transcript: Vec::new(),
            current_transcript_summarized: false,
            old_transcript_parts: Vec::new(),
            request_status: RequestStatus::NotInFlight,
            ai_execution_context: None,
        }
    }

    pub fn update_ai_execution_context(
        &mut self,
        ai_execution_context: Option<WarpAiExecutionContext>,
    ) {
        self.ai_execution_context = ai_execution_context;
    }

    /// Starts a Dwarf AI request against the server with the given request prompt.
    pub fn issue_request(&mut self, request: String, ctx: &mut ModelContext<Self>) {
        let server_api = self.server_api.clone();
        let raw_request = request.trim();
        let request_for_api = raw_request.to_string();
        let transcript = self.current_transcript.clone();
        let transcript_part_index = transcript.len();
        let ai_execution_context = self.ai_execution_context.clone();

        let request_in_markdown = markdown_segments_from_text(
            transcript_part_index,
            TranscriptPartSubType::Question,
            raw_request,
        );

        let future_handle = ctx.spawn(
            async move {
                let start_time = Utc::now();
                (start_time, server_api
                    .generate_dialogue_answer(transcript, request_for_api, ai_execution_context)
                    .await)
            },
            move |model, (start_time, response), ctx| {
                let succeeded = response.is_ok();
                let end_time = Utc::now();
                let mut current_request_status = RequestStatus::NotInFlight;
                std::mem::swap(&mut model.request_status, &mut current_request_status);
                if let RequestStatus::InFlight { request, .. } = current_request_status {
                    match response {
                        Ok(GenerateDialogueResult::Success {
                            mut answer,
                            truncated,
                            transcript_summarized,
                        }) => {
                            if truncated {
                                answer.push_str("...");
                            }

                            let trimmed_response = answer.trim();
                            let response_in_markdown = markdown_segments_from_text(
                                transcript_part_index,
                                TranscriptPartSubType::Answer,
                                trimmed_response,
                            );
                            model.current_transcript.push(TranscriptPart {
                                user: request,
                                assistant: AssistantTranscriptPart {
                                    is_error: false,
                                    copy_all_tooltip_and_button_mouse_handles: Some((Default::default(), Default::default())),
                                    formatted_message: FormattedTranscriptMessage {
                                        markdown: response_in_markdown,
                                        raw: trimmed_response.to_string(),
                                    },
                                },
                            });

                            // If the transcript was already marked as summarized before,
                            // it will remain so until it's reset.
                            model.current_transcript_summarized |= transcript_summarized;

                            let req_latency = end_time.signed_duration_since(start_time).num_milliseconds();
                        }
                        _ => {
                            let response = "We're experiencing technical difficulties right now. Please try again later.".to_owned();
                            let response_in_markdown = markdown_segments_from_text(
                                transcript_part_index,
                                TranscriptPartSubType::Answer,
                                &response,
                            );
                            model.current_transcript.push(TranscriptPart {
                                user: request,
                                assistant: AssistantTranscriptPart {
                                    is_error: true,
                                    copy_all_tooltip_and_button_mouse_handles: None,
                                    formatted_message: FormattedTranscriptMessage {
                                        markdown: response_in_markdown,
                                        raw: response,
                                    },
                                },
                            });

                        }
                    }
                }

                ctx.emit(Event::RequestFinished { succeeded });
                ctx.notify();
            },
        );

        self.request_status = RequestStatus::InFlight {
            request: FormattedTranscriptMessage {
                markdown: request_in_markdown,
                raw: raw_request.to_string(),
            },
            abort_handle: future_handle.abort_handle(),
        };

        ctx.notify();
    }

    pub fn reset(&mut self, ctx: &mut ModelContext<Self>) {
        if let RequestStatus::InFlight { abort_handle, .. } = &self.request_status {
            abort_handle.abort();
        }
        let mut old_transcript = Vec::new();
        std::mem::swap(&mut old_transcript, &mut self.current_transcript);
        self.old_transcript_parts.extend(old_transcript);
        self.request_status = RequestStatus::NotInFlight;
        self.current_transcript_summarized = false;
        ctx.notify();
    }

    pub fn transcript(&self) -> &[TranscriptPart] {
        self.current_transcript.as_slice()
    }

    /// Includes the old transcript parts appended with the current
    /// transcript parts. You likely want to just be using the current transcript parts
    /// (exposed by the `Requests::transcript` API) in most use cases.
    fn total_transcript_history(&self) -> impl Iterator<Item = &TranscriptPart> {
        self.old_transcript_parts
            .iter()
            .chain(self.current_transcript.iter())
    }

    pub fn all_past_transcript_prompts(&self) -> Vec<String> {
        self.total_transcript_history()
            .map(|p| p.raw_user_prompt().to_string())
            .collect()
    }

    /// Returns how many additional AI Assistant requests the current user can make.
    /// Free-tier quotas were removed in this fork, so we report a large constant
    /// number to keep the UI working unchanged.
    pub fn num_remaining_reqs(&self) -> usize {
        usize::MAX
    }

    pub fn request_status(&self) -> &RequestStatus {
        &self.request_status
    }

    pub fn current_transcript_summarized(&self) -> bool {
        self.current_transcript_summarized
    }
}

#[cfg(test)]
impl Requests {
    pub fn new_with_transcript(transcript: Vec<TranscriptPart>) -> Self {
        use crate::server::server_api::ServerApiProvider;

        Self {
            server_api: ServerApiProvider::new_for_test().get(),
            current_transcript: transcript,
            current_transcript_summarized: false,
            old_transcript_parts: Vec::new(),
            request_status: RequestStatus::NotInFlight,
            ai_execution_context: None,
        }
    }
}
