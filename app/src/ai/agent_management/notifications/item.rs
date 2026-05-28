use enum_iterator::Sequence;
use labrador_ui::EntityId;

use crate::ai::agent::conversation::AIConversationId;
use crate::terminal::CLIAgent;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationCategory {
    /// The agent has stopped (i.e. successfully completed or was cancelled)
    Complete,
    /// The agent needs user action (i.e. blocked on some permission request or idle prompt)
    Request,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Sequence)]
pub enum NotificationFilter {
    All,
    Unread,
    Errors,
}

/// Identifies the agent that produced a notification.
#[derive(Debug, Clone, Copy)]
#[allow(clippy::upper_case_acronyms)]
pub enum NotificationSourceAgent {
    Oz,
    CLI(CLIAgent),
}

/// Identifies the conversation or session a notification belongs to.
/// Used for de-duplication (replacing stale notifications on update)
/// and cleanup (removing notifications when the source closes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NotificationOrigin {
    Conversation(AIConversationId),
    /// CLI sessions are keyed by terminal view because we only track one session per pane.
    CLISession(EntityId),
}

#[derive(Debug, Clone)]
pub struct NotificationItem {
    pub origin: NotificationOrigin,
    pub category: NotificationCategory,
    /// Whether the user has already seen this notification
    /// (either because they clicked into it or because it was emitted for a conversation
    /// that they've since navigated to).
    pub is_read: bool,
    pub terminal_view_id: EntityId,
}

impl NotificationItem {
    pub(crate) fn new(
        category: NotificationCategory,
        origin: NotificationOrigin,
        is_read: bool,
        terminal_view_id: EntityId,
    ) -> Self {
        Self {
            origin,
            category,
            is_read,
            terminal_view_id,
        }
    }
}

#[derive(Debug, Default)]
pub struct NotificationItems {
    items: Vec<NotificationItem>,
}

impl NotificationItems {
    /// Push a notification items into the mailbox list
    /// (deleting older notifications if we've exceeded the max list size).
    pub(crate) fn push(&mut self, item: NotificationItem) {
        self.remove_by_origin(item.origin);
        self.items.insert(0, item);
        self.items.truncate(100);
    }

    pub(crate) fn remove_by_origin(&mut self, key: NotificationOrigin) -> bool {
        let before = self.items.len();
        self.items.retain(|item| item.origin != key);
        self.items.len() != before
    }

    pub(crate) fn items_filtered(
        &self,
        filter: NotificationFilter,
    ) -> impl Iterator<Item = &NotificationItem> {
        self.items.iter().filter(move |item| match filter {
            NotificationFilter::All => true,
            NotificationFilter::Unread => !item.is_read,
            NotificationFilter::Errors => item.category == NotificationCategory::Error,
        })
    }

    pub(crate) fn filtered_count(&self, filter: NotificationFilter) -> usize {
        self.items_filtered(filter).count()
    }

    pub(crate) fn has_unread_for_terminal_view(&self, terminal_view_id: EntityId) -> bool {
        self.items
            .iter()
            .any(|item| item.terminal_view_id == terminal_view_id && !item.is_read)
    }
}

#[cfg(test)]
#[path = "item_tests.rs"]
mod tests;
