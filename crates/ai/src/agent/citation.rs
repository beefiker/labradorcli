use std::fmt::Display;

use labrador_core::channel::ChannelState;
use labrador_multi_agent_api as api;

/// A citation listed in an AI response.
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub enum AIAgentCitation {
    LabradorDriveObject { uid: String },
    LabradorDocumentation { path: String },
    WebPage { url: String },
}

impl Display for AIAgentCitation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AIAgentCitation::LabradorDriveObject { uid } => {
                write!(f, "{} Object: {uid}", ChannelState::app_name_drive())
            }
            AIAgentCitation::LabradorDocumentation { path } => {
                write!(
                    f,
                    "{} Documentation: {path}",
                    ChannelState::app_name_display()
                )
            }
            AIAgentCitation::WebPage { url } => {
                write!(f, "Web Page: {url}")
            }
        }
    }
}

/// Error type for Citation conversion errors
#[derive(Debug, thiserror::Error)]
#[error("Unknown citation type")]
pub struct UnknownCitationTypeError;

fn is_labrador_drive_document_type(document_type: &api::DocumentType) -> bool {
    matches!(
        document_type,
        api::DocumentType::WarpDriveWorkflow
            | api::DocumentType::WarpDriveNotebook
            | api::DocumentType::WarpDriveEnvVar
            | api::DocumentType::Rule
    )
}

fn is_labrador_documentation_document_type(document_type: &api::DocumentType) -> bool {
    matches!(document_type, api::DocumentType::WarpDocumentation)
}

impl TryFrom<api::Citation> for AIAgentCitation {
    type Error = UnknownCitationTypeError;

    fn try_from(citation: api::Citation) -> Result<Self, Self::Error> {
        let doc_type = api::DocumentType::try_from(citation.document_type)
            .unwrap_or(api::DocumentType::Unknown);

        match doc_type {
            doc_type if is_labrador_drive_document_type(&doc_type) => {
                Ok(AIAgentCitation::LabradorDriveObject {
                    uid: citation.document_id,
                })
            }
            doc_type if is_labrador_documentation_document_type(&doc_type) => {
                Ok(AIAgentCitation::LabradorDocumentation {
                    path: citation.document_id,
                })
            }
            api::DocumentType::WebPage => Ok(AIAgentCitation::WebPage {
                url: citation.document_id,
            }),
            api::DocumentType::Unknown => Err(UnknownCitationTypeError),
            _ => Err(UnknownCitationTypeError),
        }
    }
}
