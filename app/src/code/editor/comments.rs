use warpui::Entity;

use crate::code::editor::line::EditorLineLocation;
use crate::code_review::comments::{
    AttachedReviewComment, AttachedReviewCommentTarget, CommentId,
};

#[derive(Debug, Clone)]
pub enum PendingCommentEvent {}

pub enum PendingComment {
    Closed,
    Open { line: EditorLineLocation },
}

pub struct EditorCommentsModel {
    pub pending_comment: PendingComment,
}

impl Entity for EditorCommentsModel {
    type Event = PendingCommentEvent;
}

/// Used solely at the CodeEditorView level, when we don't know
/// the file path, and later converted to a full `AttachedReviewComment`.
#[derive(Clone, Debug)]
pub struct EditorReviewComment {
    pub id: CommentId,
    pub line: EditorLineLocation,
}

impl TryFrom<AttachedReviewComment> for EditorReviewComment {
    type Error = ();

    fn try_from(comment: AttachedReviewComment) -> Result<Self, Self::Error> {
        match comment.target {
            AttachedReviewCommentTarget::Line { line, .. } => Ok(EditorReviewComment {
                id: comment.id,
                line,
            }),
            _ => Err(()),
        }
    }
}
