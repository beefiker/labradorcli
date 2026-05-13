//! The original `CommentEditor` view wrapped the now-deleted
//! `notebooks::editor::view::RichTextEditorView`. With the rich-text editor
//! and its supporting model removed from the fork, the comment composer is
//! gone. The `CommentEditorEvent` type is kept so call sites can still pattern
//! match the events that would have been emitted (none are now produced).

use crate::code::editor::line::EditorLineLocation;
use crate::code_review::comments::CommentId;

#[derive(Debug)]
pub enum CommentEditorEvent {
    ContentChanged,
    CommentSaved {
        id: Option<CommentId>,
        comment_text: String,
        #[cfg_attr(not(feature = "local_fs"), allow(dead_code))]
        line: Option<EditorLineLocation>,
    },
    CloseEditor,
    DeleteComment {
        id: CommentId,
    },
}
