use anyhow::anyhow;
use async_trait::async_trait;
use base64::Engine;
use chrono::{DateTime, Utc};
use cynic::{MutationBuilder, QueryBuilder};
use itertools::Itertools;
#[cfg(test)]
use mockall::automock;
use prost::Message;
use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};
use labrador_core::{features::FeatureFlag, report_error};
use labrador_multi_agent_api::ConversationData;

use super::auth::AuthClient;
use super::ServerApi;
use crate::ai::agent_sdk::AmbientAgentTaskId;
use crate::ai::agent::api::ServerConversationToken;
use crate::ai::agent::conversation::{AIAgentHarness, ServerAIConversationMetadata};
use crate::ai::artifacts::Artifact;
use crate::persistence::model::ConversationUsageMetadata;
use crate::terminal::model::block::SerializedBlock;
use crate::{
    ai::llms::{
        AvailableLLMs, DisableReason, LLMContextWindow, LLMInfo, LLMModelHost, LLMProvider,
        LLMSpec, LLMUsageMetadata, ModelsByFeature, RoutingHostConfig,
    },
    ai_assistant::{
        execution_context::LabradorAiExecutionContext, requests::GenerateDialogueResult,
        utils::TranscriptPart,
    },
    server::graphql::{
        default_request_options, get_request_context, get_user_facing_error_message,
    },
};
use ai::index::full_source_code_embedding::{
    self,
    store_client::{IntermediateNode, StoreClient},
    CodebaseContextConfig, ContentHash, EmbeddingConfig, NodeHash, RepoMetadata,
};
use labrador_graphql::client::Operation;
use labrador_graphql::{
    ai::{AgentTaskState, PlatformErrorCode},
    mutations::{
        confirm_file_artifact_upload::{
            ConfirmFileArtifactUpload, ConfirmFileArtifactUploadInput,
            ConfirmFileArtifactUploadResult, ConfirmFileArtifactUploadVariables,
        },
        create_file_artifact_upload_target::{
            CreateFileArtifactUploadTarget, CreateFileArtifactUploadTargetInput,
            CreateFileArtifactUploadTargetResult, CreateFileArtifactUploadTargetVariables,
        },
        delete_ai_conversation::{
            DeleteAIConversation, DeleteAIConversationVariables, DeleteConversationInput,
            DeleteConversationResult,
        },
        generate_code_embeddings::{
            GenerateCodeEmbeddings, GenerateCodeEmbeddingsInput, GenerateCodeEmbeddingsResult,
            GenerateCodeEmbeddingsVariables,
        },
        generate_dialogue::{
            GenerateDialogue, GenerateDialogueInput,
            GenerateDialogueResult as GenerateDialogueResultGraphql, GenerateDialogueStatus,
            GenerateDialogueVariables, TranscriptPart as TranscriptPartGraphql,
        },
        populate_merkle_tree_cache::{
            PopulateMerkleTreeCache, PopulateMerkleTreeCacheResult,
            PopulateMerkleTreeCacheVariables,
        },
        update_agent_task::{
            AgentTaskStatusMessageInput, UpdateAgentTask, UpdateAgentTaskInput,
            UpdateAgentTaskResult, UpdateAgentTaskVariables,
        },
        update_merkle_tree::{
            MerkleTreeNode, UpdateMerkleTree, UpdateMerkleTreeInput, UpdateMerkleTreeResult,
            UpdateMerkleTreeVariables,
        },
    },
    queries::{
        codebase_context_config::{
            CodebaseContextConfigQuery, CodebaseContextConfigResult, CodebaseContextConfigVariables,
        },
        free_available_models::{
            FreeAvailableModels, FreeAvailableModelsInput, FreeAvailableModelsResult,
            FreeAvailableModelsVariables,
        },
        get_feature_model_choices::{GetFeatureModelChoices, GetFeatureModelChoicesVariables},
        get_relevant_fragments::{
            GetRelevantFragmentsQuery, GetRelevantFragmentsResult, GetRelevantFragmentsVariables,
        },
        rerank_fragments::{RerankFragments, RerankFragmentsResult, RerankFragmentsVariables},
        sync_merkle_tree::{
            SyncMerkleTree, SyncMerkleTreeInput, SyncMerkleTreeResult, SyncMerkleTreeVariables,
        },
    },
};

const AI_ASSISTANT_REQUEST_TIMEOUT_SECONDS: u64 = 30;

/// A status update for a task, optionally including a platform error code.
pub struct TaskStatusUpdate {
    pub message: String,
    pub error_code: Option<PlatformErrorCode>,
}

impl TaskStatusUpdate {
    /// Create a status update with just a message (no error code).
    pub fn message(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            error_code: None,
        }
    }

    /// Create a status update with a message and error code.
    pub fn with_error_code(message: impl Into<String>, error_code: PlatformErrorCode) -> Self {
        Self {
            message: message.into(),
            error_code: Some(error_code),
        }
    }
}

// --- Orchestrations V2 messaging types ---

#[derive(Debug, Clone, serde::Serialize)]
pub struct SendAgentMessageRequest {
    pub to: Vec<String>,
    pub subject: String,
    pub body: String,
    pub sender_run_id: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SendAgentMessageResponse {
    pub message_ids: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AgentRunEvent {
    pub event_type: String,
    pub run_id: String,
    pub ref_id: Option<String>,
    pub execution_id: Option<String>,
    pub occurred_at: String,
    pub sequence: i64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ReadAgentMessageResponse {
    pub message_id: String,
    pub sender_run_id: String,
    pub subject: String,
    pub body: String,
    pub sent_at: String,
    pub delivered_at: Option<String>,
    pub read_at: Option<String>,
}

/// Response from the artifact endpoint.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(tag = "artifact_type")]
pub enum ArtifactDownloadResponse {
    #[serde(rename = "SCREENSHOT")]
    Screenshot {
        #[serde(flatten)]
        common: ArtifactDownloadCommonFields,
        data: ScreenshotArtifactResponseData,
    },
    #[serde(rename = "FILE")]
    File {
        #[serde(flatten)]
        common: ArtifactDownloadCommonFields,
        data: FileArtifactResponseData,
    },
}

impl ArtifactDownloadResponse {
    fn common(&self) -> &ArtifactDownloadCommonFields {
        match self {
            ArtifactDownloadResponse::Screenshot { common, .. }
            | ArtifactDownloadResponse::File { common, .. } => common,
        }
    }

    pub fn artifact_uid(&self) -> &str {
        &self.common().artifact_uid
    }

    pub fn artifact_type(&self) -> &'static str {
        match self {
            ArtifactDownloadResponse::Screenshot { .. } => "SCREENSHOT",
            ArtifactDownloadResponse::File { .. } => "FILE",
        }
    }

    pub fn created_at(&self) -> DateTime<Utc> {
        self.common().created_at
    }

    pub fn download_url(&self) -> &str {
        match self {
            ArtifactDownloadResponse::Screenshot { data, .. } => &data.download_url,
            ArtifactDownloadResponse::File { data, .. } => &data.download_url,
        }
    }

    pub fn expires_at(&self) -> DateTime<Utc> {
        match self {
            ArtifactDownloadResponse::Screenshot { data, .. } => data.expires_at,
            ArtifactDownloadResponse::File { data, .. } => data.expires_at,
        }
    }

    pub fn content_type(&self) -> &str {
        match self {
            ArtifactDownloadResponse::Screenshot { data, .. } => &data.content_type,
            ArtifactDownloadResponse::File { data, .. } => &data.content_type,
        }
    }

    pub fn filepath(&self) -> Option<&str> {
        match self {
            ArtifactDownloadResponse::Screenshot { .. } => None,
            ArtifactDownloadResponse::File { data, .. } => Some(&data.filepath),
        }
    }

    pub fn filename(&self) -> Option<&str> {
        match self {
            ArtifactDownloadResponse::Screenshot { .. } => None,
            ArtifactDownloadResponse::File { data, .. } => Some(&data.filename),
        }
    }

    pub fn description(&self) -> Option<&str> {
        match self {
            ArtifactDownloadResponse::Screenshot { data, .. } => data.description.as_deref(),
            ArtifactDownloadResponse::File { data, .. } => data.description.as_deref(),
        }
    }

    pub fn size_bytes(&self) -> Option<i64> {
        match self {
            ArtifactDownloadResponse::Screenshot { .. } => None,
            ArtifactDownloadResponse::File { data, .. } => data.size_bytes,
        }
    }
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct ArtifactDownloadCommonFields {
    pub artifact_uid: String,
    pub created_at: DateTime<Utc>,
}

/// Screenshot-specific data from the artifact endpoint.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct ScreenshotArtifactResponseData {
    pub download_url: String,
    pub expires_at: DateTime<Utc>,
    pub content_type: String,
    pub description: Option<String>,
}

/// File-specific data from the artifact endpoint.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct FileArtifactResponseData {
    pub download_url: String,
    pub expires_at: DateTime<Utc>,
    pub content_type: String,
    pub filepath: String,
    pub filename: String,
    pub description: Option<String>,
    pub size_bytes: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct CreateFileArtifactUploadRequest {
    pub conversation_id: Option<String>,
    pub run_id: Option<String>,
    pub filepath: String,
    pub description: Option<String>,
    pub mime_type: Option<String>,
    pub size_bytes: Option<i32>,
}

#[derive(Debug, Clone)]
pub struct FileArtifactRecord {
    pub artifact_uid: String,
    pub filepath: String,
    pub description: Option<String>,
    pub mime_type: String,
    pub size_bytes: Option<i32>,
}

#[derive(Debug, Clone)]
pub struct FileArtifactUploadHeaderInfo {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone)]
pub struct FileArtifactUploadTargetInfo {
    pub url: String,
    pub method: String,
    pub headers: Vec<FileArtifactUploadHeaderInfo>,
}

#[derive(Debug, Clone)]
pub struct CreateFileArtifactUploadResponse {
    pub artifact: FileArtifactRecord,
    pub upload_target: FileArtifactUploadTargetInfo,
}

#[cfg_attr(test, automock)]
#[cfg_attr(not(target_family = "wasm"), async_trait)]
#[cfg_attr(target_family = "wasm", async_trait(?Send))]
pub trait AIClient: 'static + Send + Sync {
    async fn generate_dialogue_answer(
        &self,
        transcript: Vec<TranscriptPart>,
        prompt: String,
        ai_execution_context: Option<LabradorAiExecutionContext>,
    ) -> anyhow::Result<GenerateDialogueResult>;

    async fn get_feature_model_choices(&self) -> Result<ModelsByFeature, anyhow::Error>;

    /// Fetches the free-tier available models without requiring authentication.
    /// Used during pre-login onboarding so logged-out users see an accurate model list
    /// instead of the hard-coded `ModelsByFeature::default()` fallback.
    async fn get_free_available_models(
        &self,
        referrer: Option<String>,
    ) -> Result<ModelsByFeature, anyhow::Error>;

    async fn update_agent_task(
        &self,
        task_id: AmbientAgentTaskId,
        task_state: Option<AgentTaskState>,
        session_id: Option<session_sharing_protocol::common::SessionId>,
        conversation_id: Option<String>,
        status_message: Option<TaskStatusUpdate>,
    ) -> anyhow::Result<(), anyhow::Error>;

    async fn get_ai_conversation(
        &self,
        server_conversation_token: ServerConversationToken,
    ) -> anyhow::Result<(ConversationData, ServerAIConversationMetadata), anyhow::Error>;

    async fn list_ai_conversation_metadata(
        &self,
        conversation_ids: Option<Vec<String>>,
    ) -> anyhow::Result<Vec<ServerAIConversationMetadata>>;

    async fn get_block_snapshot(
        &self,
        server_conversation_token: ServerConversationToken,
    ) -> anyhow::Result<SerializedBlock, anyhow::Error>;

    async fn delete_ai_conversation(
        &self,
        server_conversation_token: String,
    ) -> anyhow::Result<(), anyhow::Error>;

    async fn create_file_artifact_upload_target(
        &self,
        request: CreateFileArtifactUploadRequest,
    ) -> anyhow::Result<CreateFileArtifactUploadResponse, anyhow::Error>;

    async fn confirm_file_artifact_upload(
        &self,
        artifact_uid: String,
        checksum: String,
    ) -> anyhow::Result<FileArtifactRecord, anyhow::Error>;

    async fn get_artifact_download(
        &self,
        artifact_uid: &str,
    ) -> anyhow::Result<ArtifactDownloadResponse, anyhow::Error>;

    // --- Orchestrations V2 messaging ---

    async fn send_agent_message(
        &self,
        request: SendAgentMessageRequest,
    ) -> anyhow::Result<SendAgentMessageResponse, anyhow::Error>;

    /// Persists the latest observed event sequence number for a run on the
    /// server. Used to keep the server-side cursor in sync with the client so
    /// that driver/cloud restores can resume without replaying events the
    /// parent has already acted on.
    async fn update_event_sequence_on_server(
        &self,
        run_id: &str,
        sequence: i64,
    ) -> anyhow::Result<(), anyhow::Error>;

    async fn mark_message_delivered(&self, message_id: &str) -> anyhow::Result<(), anyhow::Error>;

    async fn read_agent_message(
        &self,
        message_id: &str,
    ) -> anyhow::Result<ReadAgentMessageResponse, anyhow::Error>;

}

fn into_file_artifact_record(
    artifact: labrador_graphql::mutations::create_file_artifact_upload_target::FileArtifact,
) -> FileArtifactRecord {
    FileArtifactRecord {
        artifact_uid: artifact.artifact_uid.into_inner(),
        filepath: artifact.filepath,
        description: artifact.description,
        mime_type: artifact.mime_type,
        size_bytes: artifact.size_bytes,
    }
}

#[cfg_attr(not(target_family = "wasm"), async_trait)]
#[cfg_attr(target_family = "wasm", async_trait(?Send))]
impl AIClient for ServerApi {
    async fn generate_dialogue_answer(
        &self,
        transcript: Vec<TranscriptPart>,
        prompt: String,
        // TODO: use relevant context from RequestContext and deprecate usage of ai_execution_context
        _ai_execution_context: Option<LabradorAiExecutionContext>,
    ) -> anyhow::Result<GenerateDialogueResult> {
        let graphql_transcript: Vec<TranscriptPartGraphql> = transcript
            .into_iter()
            .map(|part| TranscriptPartGraphql {
                user: part.raw_user_prompt().to_string(),
                assistant: part.raw_assistant_answer().to_string(),
            })
            .collect();
        let variables = GenerateDialogueVariables {
            input: GenerateDialogueInput {
                transcript: graphql_transcript,
                prompt,
            },
            request_context: get_request_context(),
        };

        let operation = GenerateDialogue::build(variables);
        let response = self
            .send_graphql_request(
                operation,
                Some(Duration::from_secs(AI_ASSISTANT_REQUEST_TIMEOUT_SECONDS)),
            )
            .await?;
        match response.generate_dialogue {
            GenerateDialogueResultGraphql::GenerateDialogueOutput(output) => match output.status {
                GenerateDialogueStatus::GenerateDialogueSuccess(success) => {
                    Ok(GenerateDialogueResult::Success {
                        answer: success.answer,
                        truncated: success.truncated,
                        transcript_summarized: success.transcript_summarized,
                    })
                }
                GenerateDialogueStatus::GenerateDialogueFailure(_failure) => {
                    Ok(GenerateDialogueResult::Failure)
                }
                GenerateDialogueStatus::Unknown => Err(anyhow!("failed to generate AI dialogue")),
            },
            GenerateDialogueResultGraphql::UserFacingError(e) => {
                Err(anyhow!(get_user_facing_error_message(e)))
            }
            GenerateDialogueResultGraphql::Unknown => {
                Err(anyhow!("failed to generate AI dialogue"))
            }
        }
    }

    async fn get_feature_model_choices(&self) -> Result<ModelsByFeature, anyhow::Error> {
        let variables = GetFeatureModelChoicesVariables {
            request_context: get_request_context(),
        };
        let operation = GetFeatureModelChoices::build(variables);
        let response = self.send_graphql_request(operation, None).await?;

        match response.user {
            labrador_graphql::queries::get_feature_model_choices::UserResult::UserOutput(
                labrador_graphql::queries::get_feature_model_choices::UserOutput {
                    user: labrador_graphql::queries::get_feature_model_choices::User { mut workspaces },
                },
            ) if !workspaces.is_empty() => {
                // This is safe (`remove()` can panic) because we ensure workspaces is non-empty
                // above.
                workspaces.remove(0).feature_model_choice.try_into()
            }
            _ => Err(anyhow!("Failed to get available feature model choices")),
        }
    }

    async fn get_free_available_models(
        &self,
        referrer: Option<String>,
    ) -> Result<ModelsByFeature, anyhow::Error> {
        // This resolver is public; it does not require an auth token. We must NOT go through
        // `send_graphql_request`, which awaits `get_or_refresh_access_token()`
        let variables = FreeAvailableModelsVariables {
            input: FreeAvailableModelsInput { referrer },
            request_context: get_request_context(),
        };
        let operation = FreeAvailableModels::build(variables);

        // Best-effort: if the user has a valid token (e.g. anonymous Firebase), include it;
        // otherwise send unauthenticated. Either is acceptable for this resolver.
        let auth_token = self
            .get_or_refresh_access_token()
            .await
            .ok()
            .and_then(|token| token.bearer_token());

        let response = operation
            .send_request(
                self.client.clone(),
                labrador_graphql::client::RequestOptions {
                    auth_token,
                    ..default_request_options()
                },
            )
            .await?
            .data
            .ok_or_else(|| anyhow!("Missing data in freeAvailableModels response"))?;

        match response.free_available_models {
            FreeAvailableModelsResult::FreeAvailableModelsOutput(output) => {
                output.feature_model_choice.try_into()
            }
            FreeAvailableModelsResult::Unknown => {
                Err(anyhow!("Unexpected freeAvailableModels response variant"))
            }
        }
    }

    async fn update_agent_task(
        &self,
        task_id: AmbientAgentTaskId,
        task_state: Option<AgentTaskState>,
        session_id: Option<session_sharing_protocol::common::SessionId>,
        conversation_id: Option<String>,
        status_message: Option<TaskStatusUpdate>,
    ) -> anyhow::Result<(), anyhow::Error> {
        let variables = UpdateAgentTaskVariables {
            input: UpdateAgentTaskInput {
                task_id: task_id.into(),
                task_state,
                session_id: session_id.map(|id| id.to_string().into()),
                conversation_id: conversation_id.map(|id| id.into()),
                status_message: status_message.map(|update| AgentTaskStatusMessageInput {
                    message: update.message,
                    error_code: update.error_code,
                }),
            },
            request_context: get_request_context(),
        };

        let operation = UpdateAgentTask::build(variables);
        let response = self.send_graphql_request(operation, None).await?;

        match response.update_agent_task {
            UpdateAgentTaskResult::UpdateAgentTaskOutput(_) => Ok(()),
            UpdateAgentTaskResult::UserFacingError(e) => {
                Err(anyhow!(get_user_facing_error_message(e)))
            }
            UpdateAgentTaskResult::Unknown => Err(anyhow!("failed to update agent task")),
        }
    }

    async fn get_ai_conversation(
        &self,
        server_conversation_token: ServerConversationToken,
    ) -> anyhow::Result<(ConversationData, ServerAIConversationMetadata), anyhow::Error> {
        use labrador_graphql::queries::list_ai_conversations::{
            ListAIConversations, ListAIConversationsInput, ListAIConversationsResult,
            ListAIConversationsVariables,
        };

        let conversation_id = server_conversation_token.as_str().to_string();
        let operation = ListAIConversations::build(ListAIConversationsVariables {
            input: ListAIConversationsInput {
                conversation_ids: Some(vec![cynic::Id::new(conversation_id)]),
            },
            request_context: get_request_context(),
        });
        let response = self.send_graphql_request(operation, None).await?;

        let gql_conversation = match response.list_ai_conversations {
            ListAIConversationsResult::ListAIConversationsOutput(output) => output
                .conversations
                .into_iter()
                .next()
                .ok_or_else(|| anyhow!("Conversation not found"))?,
            ListAIConversationsResult::UserFacingError(e) => {
                return Err(anyhow!(get_user_facing_error_message(e)));
            }
            ListAIConversationsResult::Unknown => {
                return Err(anyhow!("Failed to get AI conversation"));
            }
        };

        let conversation_data_bytes = base64::engine::general_purpose::STANDARD
            .decode(&gql_conversation.final_task_list)
            .map_err(|e| anyhow!("Failed to decode base64 conversation data: {e}"))?;

        let conversation_data = ConversationData::decode(conversation_data_bytes.as_slice())
            .map_err(|e| anyhow!("Failed to decode proto ConversationData: {e}"))?;

        // Build AIConversationMetadata from GraphQL response
        let metadata = gql_conversation.try_into()?;

        Ok((conversation_data, metadata))
    }

    async fn list_ai_conversation_metadata(
        &self,
        conversation_ids: Option<Vec<String>>,
    ) -> anyhow::Result<Vec<ServerAIConversationMetadata>> {
        if !FeatureFlag::CloudConversations.is_enabled() {
            return Ok(vec![]);
        }
        use labrador_graphql::queries::list_ai_conversations::{
            ListAIConversationMetadata, ListAIConversationMetadataResult,
            ListAIConversationMetadataVariables, ListAIConversationsInput,
        };

        let input = ListAIConversationsInput {
            conversation_ids: conversation_ids
                .map(|ids| ids.into_iter().map(cynic::Id::new).collect()),
        };

        let variables = ListAIConversationMetadataVariables {
            input,
            request_context: get_request_context(),
        };

        let operation = ListAIConversationMetadata::build(variables);
        let response = self.send_graphql_request(operation, None).await?;

        match response.list_ai_conversations {
            ListAIConversationMetadataResult::ListAIConversationsOutput(output) => {
                let metadata_vec: Result<Vec<_>, _> = output
                    .conversations
                    .into_iter()
                    .map(|conv| conv.try_into())
                    .collect();
                metadata_vec
            }
            ListAIConversationMetadataResult::UserFacingError(e) => {
                Err(anyhow!(get_user_facing_error_message(e)))
            }
            ListAIConversationMetadataResult::Unknown => {
                Err(anyhow!("Failed to list AI conversations metadata"))
            }
        }
    }

    async fn get_block_snapshot(
        &self,
        server_conversation_token: ServerConversationToken,
    ) -> anyhow::Result<SerializedBlock, anyhow::Error> {
        let conversation_id = server_conversation_token.as_str();
        // Make sure to use `SerializedBlock::from_json` to correctly handle the serialized
        // command and output grid contents.
        let response = self
            .get_public_api_response(&format!(
                "agent/conversations/{conversation_id}/block-snapshot"
            ))
            .await?;
        let json_bytes = response
            .bytes()
            .await
            .map_err(|e| anyhow!("Failed to read block snapshot for {conversation_id}: {e}"))?;
        SerializedBlock::from_json(&json_bytes)
    }

    async fn delete_ai_conversation(
        &self,
        server_conversation_token: String,
    ) -> anyhow::Result<(), anyhow::Error> {
        let variables = DeleteAIConversationVariables {
            input: DeleteConversationInput {
                conversation_id: server_conversation_token.into(),
            },
            request_context: get_request_context(),
        };

        let operation = DeleteAIConversation::build(variables);
        let response = self.send_graphql_request(operation, None).await?;

        match response.delete_conversation {
            DeleteConversationResult::DeleteConversationOutput(_) => Ok(()),
            DeleteConversationResult::UserFacingError(e) => {
                Err(anyhow!(get_user_facing_error_message(e)))
            }
            DeleteConversationResult::Unknown => Err(anyhow!("Failed to delete AI conversation")),
        }
    }

    async fn create_file_artifact_upload_target(
        &self,
        request: CreateFileArtifactUploadRequest,
    ) -> anyhow::Result<CreateFileArtifactUploadResponse, anyhow::Error> {
        let variables = CreateFileArtifactUploadTargetVariables {
            input: CreateFileArtifactUploadTargetInput {
                conversation_id: request.conversation_id.map(cynic::Id::new),
                run_id: request.run_id.map(cynic::Id::new),
                filepath: request.filepath,
                description: request.description,
                mime_type: request.mime_type,
                size_bytes: request.size_bytes,
            },
            request_context: get_request_context(),
        };
        let operation = CreateFileArtifactUploadTarget::build(variables);
        let response = self.send_graphql_request(operation, None).await?;

        match response.create_file_artifact_upload_target {
            CreateFileArtifactUploadTargetResult::CreateFileArtifactUploadTargetOutput(output) => {
                Ok(CreateFileArtifactUploadResponse {
                    artifact: into_file_artifact_record(output.artifact),
                    upload_target: FileArtifactUploadTargetInfo {
                        url: output.upload_target.url,
                        method: output.upload_target.method,
                        headers: output
                            .upload_target
                            .headers
                            .into_iter()
                            .map(|header| FileArtifactUploadHeaderInfo {
                                name: header.name,
                                value: header.value,
                            })
                            .collect(),
                    },
                })
            }
            CreateFileArtifactUploadTargetResult::UserFacingError(error) => {
                Err(anyhow!(get_user_facing_error_message(error)))
            }
            CreateFileArtifactUploadTargetResult::Unknown => {
                Err(anyhow!("Failed to create file artifact upload target"))
            }
        }
    }

    async fn confirm_file_artifact_upload(
        &self,
        artifact_uid: String,
        checksum: String,
    ) -> anyhow::Result<FileArtifactRecord, anyhow::Error> {
        let variables = ConfirmFileArtifactUploadVariables {
            input: ConfirmFileArtifactUploadInput {
                artifact_uid: cynic::Id::new(artifact_uid),
                checksum,
            },
            request_context: get_request_context(),
        };
        let operation = ConfirmFileArtifactUpload::build(variables);
        let response = self.send_graphql_request(operation, None).await?;

        match response.confirm_file_artifact_upload {
            ConfirmFileArtifactUploadResult::ConfirmFileArtifactUploadOutput(output) => {
                Ok(into_file_artifact_record(output.artifact))
            }
            ConfirmFileArtifactUploadResult::UserFacingError(error) => {
                Err(anyhow!(get_user_facing_error_message(error)))
            }
            ConfirmFileArtifactUploadResult::Unknown => {
                Err(anyhow!("Failed to confirm file artifact upload"))
            }
        }
    }

    async fn get_artifact_download(
        &self,
        artifact_uid: &str,
    ) -> anyhow::Result<ArtifactDownloadResponse, anyhow::Error> {
        let response: ArtifactDownloadResponse = self
            .get_public_api(&format!("agent/artifacts/{artifact_uid}"))
            .await?;
        Ok(response)
    }

    // --- Orchestrations V2 messaging ---

    async fn send_agent_message(
        &self,
        request: SendAgentMessageRequest,
    ) -> anyhow::Result<SendAgentMessageResponse, anyhow::Error> {
        let response: SendAgentMessageResponse =
            self.post_public_api("agent/messages", &request).await?;
        Ok(response)
    }

    async fn update_event_sequence_on_server(
        &self,
        run_id: &str,
        sequence: i64,
    ) -> anyhow::Result<(), anyhow::Error> {
        #[derive(serde::Serialize)]
        struct UpdateBody {
            sequence: i64,
        }
        self.patch_public_api_unit(
            &format!("agent/runs/{run_id}/event-sequence"),
            &UpdateBody { sequence },
        )
        .await
    }

    async fn mark_message_delivered(&self, message_id: &str) -> anyhow::Result<(), anyhow::Error> {
        self.post_public_api_unit(&format!("agent/messages/{message_id}/delivered"), &())
            .await
    }

    async fn read_agent_message(
        &self,
        message_id: &str,
    ) -> anyhow::Result<ReadAgentMessageResponse, anyhow::Error> {
        let response: ReadAgentMessageResponse = self
            .post_public_api(&format!("agent/messages/{message_id}/read"), &())
            .await?;
        Ok(response)
    }

}

impl TryFrom<labrador_graphql::queries::get_feature_model_choices::FeatureModelChoice>
    for ModelsByFeature
{
    type Error = anyhow::Error;

    fn try_from(
        value: labrador_graphql::queries::get_feature_model_choices::FeatureModelChoice,
    ) -> Result<Self, Self::Error> {
        Ok(Self {
            agent_mode: value.agent_mode.try_into()?,
            coding: value.coding.try_into()?,
            cli_agent: Some(value.cli_agent.try_into()?),
            computer_use: Some(value.computer_use_agent.try_into()?),
        })
    }
}

impl TryFrom<labrador_graphql::workspace::FeatureModelChoice> for ModelsByFeature {
    type Error = anyhow::Error;

    fn try_from(value: labrador_graphql::workspace::FeatureModelChoice) -> Result<Self, Self::Error> {
        Ok(Self {
            agent_mode: value.agent_mode.try_into()?,
            coding: value.coding.try_into()?,
            cli_agent: Some(value.cli_agent.try_into()?),
            computer_use: Some(value.computer_use_agent.try_into()?),
        })
    }
}

impl TryFrom<labrador_graphql::queries::get_feature_model_choices::AvailableLlms> for AvailableLLMs {
    type Error = anyhow::Error;

    fn try_from(
        value: labrador_graphql::queries::get_feature_model_choices::AvailableLlms,
    ) -> Result<Self, Self::Error> {
        Self::new(
            value.default_id.into(),
            value.choices.into_iter().map(LLMInfo::from),
            value.preferred_codex_model_id.map(Into::into),
        )
    }
}

impl TryFrom<labrador_graphql::workspace::AvailableLlms> for AvailableLLMs {
    type Error = anyhow::Error;

    fn try_from(value: labrador_graphql::workspace::AvailableLlms) -> Result<Self, Self::Error> {
        Self::new(
            value.default_id.into(),
            value.choices.into_iter().map(LLMInfo::from),
            value.preferred_codex_model_id.map(Into::into),
        )
    }
}

impl From<labrador_graphql::queries::get_feature_model_choices::LlmInfo> for LLMInfo {
    fn from(value: labrador_graphql::queries::get_feature_model_choices::LlmInfo) -> Self {
        let host_configs = {
            let mut map = std::collections::HashMap::new();
            for config in value.host_configs {
                let config: RoutingHostConfig = config.into();
                let host = config.model_routing_host.clone();
                if map.insert(host.clone(), config).is_some() {
                    log::warn!(
                        "Duplicate LlmModelHost entry for {:?}, using latest value",
                        host
                    );
                }
            }
            map
        };
        Self {
            id: value.id.into(),
            display_name: value.display_name,
            base_model_name: value.base_model_name,
            reasoning_level: value.reasoning_level,
            usage_metadata: value.usage_metadata.into(),
            description: value.description,
            disable_reason: value.disable_reason.map(DisableReason::from),
            vision_supported: value.vision_supported,
            spec: value.spec.map(Into::into),
            provider: value.provider.into(),
            host_configs,
            discount_percentage: value.pricing.discount_percentage.map(|v| v as f32),
            context_window: LLMContextWindow {
                is_configurable: value.context_window.is_configurable,
                min: value.context_window.min.into(),
                max: value.context_window.max.into(),
                default_max: value.context_window.default.into(),
            },
        }
    }
}

impl From<labrador_graphql::workspace::LlmInfo> for LLMInfo {
    fn from(value: labrador_graphql::workspace::LlmInfo) -> Self {
        let host_configs = {
            let mut map = std::collections::HashMap::new();
            for config in value.host_configs {
                let config: RoutingHostConfig = config.into();
                let host = config.model_routing_host.clone();
                if map.insert(host.clone(), config).is_some() {
                    log::warn!(
                        "Duplicate LlmModelHost entry for {:?}, using latest value",
                        host
                    );
                }
            }
            map
        };
        Self {
            id: value.id.into(),
            display_name: value.display_name,
            base_model_name: value.base_model_name,
            reasoning_level: value.reasoning_level,
            usage_metadata: value.usage_metadata.into(),
            description: value.description,
            disable_reason: value.disable_reason.map(DisableReason::from),
            vision_supported: value.vision_supported,
            spec: value.spec.map(Into::into),
            provider: value.provider.into(),
            host_configs,
            discount_percentage: value.pricing.discount_percentage.map(|v| v as f32),
            context_window: LLMContextWindow {
                is_configurable: value.context_window.is_configurable,
                min: value.context_window.min.into(),
                max: value.context_window.max.into(),
                default_max: value.context_window.default.into(),
            },
        }
    }
}

impl From<labrador_graphql::queries::get_feature_model_choices::RoutingHostConfig>
    for RoutingHostConfig
{
    fn from(value: labrador_graphql::queries::get_feature_model_choices::RoutingHostConfig) -> Self {
        Self {
            enabled: value.enabled,
            model_routing_host: value.model_routing_host.into(),
        }
    }
}

impl From<labrador_graphql::workspace::RoutingHostConfig> for RoutingHostConfig {
    fn from(value: labrador_graphql::workspace::RoutingHostConfig) -> Self {
        Self {
            enabled: value.enabled,
            model_routing_host: value.model_routing_host.into(),
        }
    }
}

impl From<labrador_graphql::queries::get_feature_model_choices::LlmModelHost> for LLMModelHost {
    fn from(value: labrador_graphql::queries::get_feature_model_choices::LlmModelHost) -> Self {
        match value {
            labrador_graphql::queries::get_feature_model_choices::LlmModelHost::DirectApi => {
                LLMModelHost::DirectApi
            }
            labrador_graphql::queries::get_feature_model_choices::LlmModelHost::AwsBedrock => {
                LLMModelHost::AwsBedrock
            }
            labrador_graphql::queries::get_feature_model_choices::LlmModelHost::Other(value) => {
                report_error!(anyhow!(
                    "Unknown LlmModelHost '{value}'. Make sure to update client GraphQL types!"
                ));
                LLMModelHost::Unknown
            }
        }
    }
}

impl From<labrador_graphql::queries::get_feature_model_choices::LlmProvider> for LLMProvider {
    fn from(value: labrador_graphql::queries::get_feature_model_choices::LlmProvider) -> Self {
        match value {
            labrador_graphql::queries::get_feature_model_choices::LlmProvider::Openai => {
                LLMProvider::OpenAI
            }
            labrador_graphql::queries::get_feature_model_choices::LlmProvider::Anthropic => {
                LLMProvider::Anthropic
            }
            labrador_graphql::queries::get_feature_model_choices::LlmProvider::Google => {
                LLMProvider::Google
            }
            labrador_graphql::queries::get_feature_model_choices::LlmProvider::Xai => LLMProvider::Xai,
            labrador_graphql::queries::get_feature_model_choices::LlmProvider::Unknown => {
                LLMProvider::Unknown
            }
            labrador_graphql::queries::get_feature_model_choices::LlmProvider::Other(value) => {
                report_error!(anyhow!(
                    "Invalid LlmProvider '{value}'. Make sure to update client GraphQL types!"
                ));
                LLMProvider::Unknown
            }
        }
    }
}

impl From<labrador_graphql::workspace::LlmProvider> for LLMProvider {
    fn from(value: labrador_graphql::workspace::LlmProvider) -> Self {
        match value {
            labrador_graphql::workspace::LlmProvider::Openai => LLMProvider::OpenAI,
            labrador_graphql::workspace::LlmProvider::Anthropic => LLMProvider::Anthropic,
            labrador_graphql::workspace::LlmProvider::Google => LLMProvider::Google,
            labrador_graphql::workspace::LlmProvider::Xai => LLMProvider::Xai,
            labrador_graphql::workspace::LlmProvider::Unknown => LLMProvider::Unknown,
            labrador_graphql::workspace::LlmProvider::Other(value) => {
                report_error!(anyhow!(
                    "Invalid LlmProvider '{value}'. Make sure to update client GraphQL types!"
                ));
                LLMProvider::Unknown
            }
        }
    }
}

impl From<labrador_graphql::queries::get_feature_model_choices::LlmSpec> for LLMSpec {
    fn from(value: labrador_graphql::queries::get_feature_model_choices::LlmSpec) -> Self {
        Self {
            cost: value.cost as f32,
            quality: value.quality as f32,
            speed: value.speed as f32,
        }
    }
}

impl From<labrador_graphql::workspace::LlmSpec> for LLMSpec {
    fn from(value: labrador_graphql::workspace::LlmSpec) -> Self {
        Self {
            cost: value.cost as f32,
            quality: value.quality as f32,
            speed: value.speed as f32,
        }
    }
}

impl From<labrador_graphql::queries::get_feature_model_choices::LlmUsageMetadata> for LLMUsageMetadata {
    fn from(value: labrador_graphql::queries::get_feature_model_choices::LlmUsageMetadata) -> Self {
        Self {
            request_multiplier: value.request_multiplier.max(1) as usize,
            credit_multiplier: value.credit_multiplier.map(|v| v as f32),
        }
    }
}

impl From<labrador_graphql::workspace::LlmUsageMetadata> for LLMUsageMetadata {
    fn from(value: labrador_graphql::workspace::LlmUsageMetadata) -> Self {
        Self {
            request_multiplier: value.request_multiplier.max(1) as usize,
            credit_multiplier: value.credit_multiplier.map(|v| v as f32),
        }
    }
}

impl From<labrador_graphql::queries::get_feature_model_choices::DisableReason> for DisableReason {
    fn from(value: labrador_graphql::queries::get_feature_model_choices::DisableReason) -> Self {
        match value {
            labrador_graphql::queries::get_feature_model_choices::DisableReason::AdminDisabled => {
                DisableReason::AdminDisabled
            }
            labrador_graphql::queries::get_feature_model_choices::DisableReason::OutOfRequests => {
                DisableReason::OutOfRequests
            }
            labrador_graphql::queries::get_feature_model_choices::DisableReason::ProviderOutage => {
                DisableReason::ProviderOutage
            }
            labrador_graphql::queries::get_feature_model_choices::DisableReason::RequiresUpgrade => {
                DisableReason::RequiresUpgrade
            }
            labrador_graphql::queries::get_feature_model_choices::DisableReason::Other(_) => {
                DisableReason::Unavailable
            }
        }
    }
}

impl From<labrador_graphql::workspace::DisableReason> for DisableReason {
    fn from(value: labrador_graphql::workspace::DisableReason) -> Self {
        match value {
            labrador_graphql::workspace::DisableReason::AdminDisabled => DisableReason::AdminDisabled,
            labrador_graphql::workspace::DisableReason::OutOfRequests => DisableReason::OutOfRequests,
            labrador_graphql::workspace::DisableReason::ProviderOutage => DisableReason::ProviderOutage,
            labrador_graphql::workspace::DisableReason::RequiresUpgrade => {
                DisableReason::RequiresUpgrade
            }
            labrador_graphql::workspace::DisableReason::Other(_) => DisableReason::Unavailable,
        }
    }
}

// Conversions for AIConversationMetadata from GraphQL types

fn convert_harness(harness: labrador_graphql::ai::AgentHarness) -> AIAgentHarness {
    match harness {
        labrador_graphql::ai::AgentHarness::Oz => AIAgentHarness::Oz,
        labrador_graphql::ai::AgentHarness::ClaudeCode => AIAgentHarness::ClaudeCode,
        labrador_graphql::ai::AgentHarness::Gemini => AIAgentHarness::Gemini,
        labrador_graphql::ai::AgentHarness::Codex => AIAgentHarness::Codex,
        labrador_graphql::ai::AgentHarness::Other(value) => {
            report_error!(anyhow!(
                "Invalid AgentHarness '{value}'. Make sure to update client GraphQL types!"
            ));
            AIAgentHarness::Unknown
        }
    }
}

// Helper function
fn convert_usage_metadata(
    summarized: bool,
    context_window_usage: f64,
    credits_spent: f64,
) -> ConversationUsageMetadata {
    ConversationUsageMetadata {
        was_summarized: summarized,
        context_window_usage: context_window_usage as f32,
        credits_spent: credits_spent as f32,
        credits_spent_for_last_block: None,
        token_usage: vec![],
        tool_usage_metadata: Default::default(),
    }
}

impl TryFrom<labrador_graphql::ai::AIConversation> for ServerAIConversationMetadata {
    type Error = anyhow::Error;

    fn try_from(value: labrador_graphql::ai::AIConversation) -> Result<Self, Self::Error> {
        let usage = convert_usage_metadata(
            value.usage.usage_metadata.summarized,
            value.usage.usage_metadata.context_window_usage,
            value.usage.usage_metadata.credits_spent,
        );
        let ambient_agent_task_id = value
            .ambient_agent_task_id
            .map(|id| id.into_inner().parse())
            .transpose()?;
        let server_conversation_token =
            ServerConversationToken::new(value.conversation_id.into_inner());

        // If we fail to parse any artifacts, don't fail the entire conversion -- just don't include them in the list
        let artifacts = value
            .artifacts
            .unwrap_or_default()
            .into_iter()
            .filter_map(|a| Artifact::try_from(a).ok())
            .collect();

        Ok(Self {
            title: value.title,
            working_directory: value.working_directory,
            harness: convert_harness(value.harness),
            usage,
            ambient_agent_task_id,
            server_conversation_token,
            artifacts,
        })
    }
}

impl TryFrom<labrador_graphql::queries::list_ai_conversations::AIConversationMetadata>
    for ServerAIConversationMetadata
{
    type Error = anyhow::Error;

    fn try_from(
        value: labrador_graphql::queries::list_ai_conversations::AIConversationMetadata,
    ) -> Result<Self, Self::Error> {
        let usage = convert_usage_metadata(
            value.usage.usage_metadata.summarized,
            value.usage.usage_metadata.context_window_usage,
            value.usage.usage_metadata.credits_spent,
        );
        let ambient_agent_task_id = value
            .ambient_agent_task_id
            .map(|id| id.into_inner().parse())
            .transpose()?;
        let server_conversation_token =
            ServerConversationToken::new(value.conversation_id.into_inner());

        let artifacts = value
            .artifacts
            .unwrap_or_default()
            .into_iter()
            .filter_map(|a| Artifact::try_from(a).ok())
            .collect();

        Ok(Self {
            title: value.title,
            working_directory: value.working_directory,
            harness: convert_harness(value.harness),
            usage,
            ambient_agent_task_id,
            server_conversation_token,
            artifacts,
        })
    }
}

#[cfg_attr(not(target_family = "wasm"), async_trait)]
#[cfg_attr(target_family = "wasm", async_trait(?Send))]
impl StoreClient for ServerApi {
    async fn update_intermediate_nodes(
        &self,
        embedding_config: EmbeddingConfig,
        nodes: Vec<IntermediateNode>,
    ) -> Result<HashMap<NodeHash, bool>, full_source_code_embedding::Error> {
        let nodes = nodes
            .into_iter()
            .map(|node| MerkleTreeNode {
                hash: node.hash.into(),
                children: node.children.into_iter().map(Into::into).collect(),
            })
            .collect_vec();
        let variables = UpdateMerkleTreeVariables {
            input: UpdateMerkleTreeInput {
                embedding_config: embedding_config.into(),
                nodes,
            },
            request_context: get_request_context(),
        };
        let operation = UpdateMerkleTree::build(variables);
        let response = self.send_graphql_request(operation, None).await?;

        match response.update_merkle_tree {
            UpdateMerkleTreeResult::UpdateMerkleTreeOutput(output) => {
                let mut node_results = HashMap::with_capacity(output.results.len());
                for result in output.results {
                    node_results.insert(result.hash.try_into()?, result.success);
                }
                Ok(node_results)
            }
            UpdateMerkleTreeResult::UpdateMerkleTreeError(e) => Err(anyhow!(e.error).into()),
            UpdateMerkleTreeResult::UserFacingError(e) => {
                Err(anyhow!(get_user_facing_error_message(e)).into())
            }
            UpdateMerkleTreeResult::Unknown => {
                Err(anyhow!("failed to update merkle tree").into())
            }
        }
    }

    async fn generate_embeddings(
        &self,
        embedding_config: EmbeddingConfig,
        fragments: Vec<full_source_code_embedding::Fragment>,
        root_hash: NodeHash,
        repo_metadata: RepoMetadata,
    ) -> Result<HashMap<ContentHash, bool>, full_source_code_embedding::Error> {
        let variables = GenerateCodeEmbeddingsVariables {
            input: GenerateCodeEmbeddingsInput {
                embedding_config: embedding_config.into(),
                fragments: fragments.into_iter().map(Into::into).collect(),
                repo_metadata: repo_metadata.into(),
                root_hash: root_hash.into(),
            },
            request_context: get_request_context(),
        };

        let operation = GenerateCodeEmbeddings::build(variables);
        let response = self.send_graphql_request(operation, None).await?;

        match response.generate_code_embeddings {
            GenerateCodeEmbeddingsResult::GenerateCodeEmbeddingsOutput(output) => {
                let mut results = HashMap::with_capacity(output.embedding_results.len());
                for result in output.embedding_results {
                    results.insert(result.hash.try_into()?, result.success);
                }
                Ok(results)
            }
            GenerateCodeEmbeddingsResult::GenerateCodeEmbeddingsError(e) => {
                Err(anyhow!(e.error).into())
            }
            GenerateCodeEmbeddingsResult::UserFacingError(e) => {
                Err(anyhow!(get_user_facing_error_message(e)).into())
            }
            GenerateCodeEmbeddingsResult::Unknown => {
                Err(anyhow!("failed to generate code embeddings").into())
            }
        }
    }

    async fn populate_merkle_tree_cache(
        &self,
        embedding_config: EmbeddingConfig,
        root_hash: NodeHash,
        repo_metadata: RepoMetadata,
    ) -> Result<bool, full_source_code_embedding::Error> {
        let variables = PopulateMerkleTreeCacheVariables {
            embedding_config: embedding_config.into(),
            root_hash: root_hash.into(),
            repo_metadata: repo_metadata.into(),
            request_context: get_request_context(),
        };
        let operation = PopulateMerkleTreeCache::build(variables);
        let response = self.send_graphql_request(operation, None).await?;

        match response.populate_merkle_tree_cache {
            PopulateMerkleTreeCacheResult::PopulateMerkleTreeCacheOutput(output) => {
                Ok(output.success)
            }
            PopulateMerkleTreeCacheResult::UserFacingError(e) => {
                Err(anyhow!(get_user_facing_error_message(e)).into())
            }
            PopulateMerkleTreeCacheResult::Unknown => {
                Err(anyhow!("failed to populate merkle tree cache").into())
            }
        }
    }

    async fn sync_merkle_tree(
        &self,
        nodes: Vec<NodeHash>,
        embedding_config: EmbeddingConfig,
    ) -> Result<HashSet<NodeHash>, full_source_code_embedding::Error> {
        let input = SyncMerkleTreeInput {
            hashed_nodes: nodes.into_iter().map(Into::into).collect(),
            embedding_config: embedding_config.into(),
        };

        let variables = SyncMerkleTreeVariables {
            input,
            request_context: get_request_context(),
        };

        let operation = SyncMerkleTree::build(variables);
        let response = self.send_graphql_request(operation, None).await?;

        match response.sync_merkle_tree {
            SyncMerkleTreeResult::SyncMerkleTreeOutput(output) => {
                let mut node_results = HashSet::with_capacity(output.changed_nodes.len());
                for hash in output.changed_nodes {
                    node_results.insert(hash.try_into()?);
                }
                Ok(node_results)
            }
            SyncMerkleTreeResult::SyncMerkleTreeError(e) => Err(anyhow!(e.error).into()),
            SyncMerkleTreeResult::UserFacingError(e) => {
                Err(anyhow!(get_user_facing_error_message(e)).into())
            }
            SyncMerkleTreeResult::Unknown => Err(anyhow!("failed to sync merkle tree").into()),
        }
    }

    async fn rerank_fragments(
        &self,
        query: String,
        fragments: Vec<full_source_code_embedding::Fragment>,
    ) -> Result<Vec<full_source_code_embedding::Fragment>, full_source_code_embedding::Error> {
        let variables = RerankFragmentsVariables {
            query,
            fragments: fragments.into_iter().map(Into::into).collect(),
            request_context: get_request_context(),
        };
        let operation = RerankFragments::build(variables);
        let response = self.send_graphql_request(operation, None).await?;

        match response.rerank_fragments {
            RerankFragmentsResult::RerankFragmentsOutput(output) => Ok(output
                .ranked_fragments
                .into_iter()
                .map(|fragment| fragment.try_into())
                .collect::<Result<Vec<_>, _>>()?),
            RerankFragmentsResult::RerankFragmentsError(e) => Err(anyhow!(e.error).into()),
            RerankFragmentsResult::UserFacingError(e) => {
                Err(anyhow!(get_user_facing_error_message(e)).into())
            }
            RerankFragmentsResult::Unknown => Err(anyhow!("failed to rerank fragments").into()),
        }
    }

    async fn get_relevant_fragments(
        &self,
        embedding_config: EmbeddingConfig,
        query: String,
        root_hash: NodeHash,
        repo_metadata: RepoMetadata,
    ) -> Result<Vec<ContentHash>, full_source_code_embedding::Error> {
        let variables = GetRelevantFragmentsVariables {
            query,
            root_hash: root_hash.into(),
            embedding_config: embedding_config.into(),
            request_context: get_request_context(),
            repo_metadata: repo_metadata.into(),
        };
        let operation = GetRelevantFragmentsQuery::build(variables);
        let response = self.send_graphql_request(operation, None).await?;

        match response.get_relevant_fragments {
            GetRelevantFragmentsResult::GetRelevantFragmentsOutput(output) => Ok(output
                .candidate_hashes
                .into_iter()
                .map(|hash| hash.try_into())
                .collect::<Result<Vec<_>, _>>()?),
            GetRelevantFragmentsResult::UserFacingError(e) => {
                Err(anyhow!(get_user_facing_error_message(e)).into())
            }
            GetRelevantFragmentsResult::GetRelevantFragmentsError(e) => {
                Err(anyhow!(e.error).into())
            }
            GetRelevantFragmentsResult::Unknown => {
                Err(anyhow!("failed to get relevant fragments").into())
            }
        }
    }

    async fn codebase_context_config(
        &self,
    ) -> Result<CodebaseContextConfig, full_source_code_embedding::Error> {
        let variables = CodebaseContextConfigVariables {
            request_context: get_request_context(),
        };
        let operation = CodebaseContextConfigQuery::build(variables);
        let response = self.send_graphql_request(operation, None).await?;

        match response.codebase_context_config {
            CodebaseContextConfigResult::CodebaseContextConfigOutput(output) => {
                Ok(CodebaseContextConfig {
                    embedding_config: output.embedding_config.try_into()?,
                    embedding_cadence: Duration::from_secs(output.embedding_cadence as u64),
                })
            }
            CodebaseContextConfigResult::UserFacingError(e) => {
                Err(anyhow!(get_user_facing_error_message(e)).into())
            }
            CodebaseContextConfigResult::Unknown => {
                Err(anyhow!("failed to retrieve codebase context config").into())
            }
        }
    }
}

