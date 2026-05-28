//! Module to attribute AI-generated requested commands
//! to known documents. The Labrador Drive integration has been removed,
//! so this stub returns false for all citations.

use labrador_ui::AppContext;

use crate::ai::agent::AIAgentCitation;
use crate::terminal::shell::ShellType;

/// Returns true iff the `command` is directly copied from the `document`.
pub(crate) fn is_command_copied_from_document(
    _command: &str,
    _document: &AIAgentCitation,
    _shell_type: Option<ShellType>,
    _ctx: &AppContext,
) -> bool {
    false
}
