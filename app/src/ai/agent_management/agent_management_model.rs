use std::collections::HashMap;

use warpui::{AppContext, Entity, EntityId, ModelContext, SingletonEntity, WindowId};

use crate::settings::AISettings;

use crate::ai::active_agent_views_model::{ActiveAgentViewsEvent, ActiveAgentViewsModel};
use crate::ai::agent::conversation::{AIConversationId, ConversationStatus};
use crate::ai::agent_management::notifications::{
    NotificationCategory, NotificationId, NotificationItem, NotificationItems, NotificationOrigin,
};
use crate::ai::artifacts::Artifact;
use crate::ai::blocklist::BlocklistAIHistoryEvent;
use crate::terminal::cli_agent_sessions::{
    CLIAgentSessionStatus, CLIAgentSessionsModel, CLIAgentSessionsModelEvent,
};
use crate::workspace::util::is_terminal_view_in_same_tab;
use crate::workspace::{Workspace, WorkspaceRegistry};
use crate::BlocklistAIHistoryModel;

/// Singleton model responsible for triggering in-app notifications on blocking conversation
/// status updates and tracking/storing these notifications for the notifications mailbox.
/// Tracks and stores notifications for both warp agent conversations and other supported
/// cli agent sessions.
pub struct AgentNotificationsModel {
    notifications: NotificationItems,
    /// Artifacts accumulated during the current turn for each conversation.
    /// Drained into the notification when a terminal state fires, cleared on InProgress.
    pub(crate) pending_artifacts: HashMap<AIConversationId, Vec<Artifact>>,
}

impl Entity for AgentNotificationsModel {
    type Event = AgentManagementEvent;
}

impl SingletonEntity for AgentNotificationsModel {}

impl AgentNotificationsModel {
    pub(crate) fn new(ctx: &mut ModelContext<Self>) -> Self {
        let history_model = BlocklistAIHistoryModel::handle(ctx);
        ctx.subscribe_to_model(&history_model, move |me, event, ctx| {
            me.handle_history_event(event, ctx);
        });

        let cli_sessions_model = CLIAgentSessionsModel::handle(ctx);
        ctx.subscribe_to_model(&cli_sessions_model, |me, event, ctx| {
            me.handle_cli_agent_session_event(event, ctx);
        });

        let active_views_model = ActiveAgentViewsModel::handle(ctx);
        ctx.subscribe_to_model(&active_views_model, |me, event, ctx| {
            me.handle_active_agent_views_changed(event, ctx);
        });

        Self {
            notifications: NotificationItems::default(),
            pending_artifacts: HashMap::new(),
        }
    }

    pub(crate) fn notifications(&self) -> &NotificationItems {
        &self.notifications
    }

    /// Marks all notifications from the given terminal view as read.
    pub(crate) fn mark_items_from_terminal_view_read(
        &mut self,
        _terminal_view_id: EntityId,
        _ctx: &mut ModelContext<Self>,
    ) {
        // HOA notifications feature removed; no-op.
    }

    fn handle_active_agent_views_changed(
        &mut self,
        _event: &ActiveAgentViewsEvent,
        _ctx: &mut ModelContext<Self>,
    ) {
        // HOA notifications feature removed; no-op.
    }

    fn handle_cli_agent_session_event(
        &mut self,
        event: &CLIAgentSessionsModelEvent,
        ctx: &mut ModelContext<Self>,
    ) {
        match event {
            CLIAgentSessionsModelEvent::Ended {
                terminal_view_id, ..
            } => {
                self.remove_notification_by_source(
                    NotificationOrigin::CLISession(*terminal_view_id),
                    ctx,
                );
            }
            CLIAgentSessionsModelEvent::Started { .. }
            | CLIAgentSessionsModelEvent::InputSessionChanged { .. }
            | CLIAgentSessionsModelEvent::SessionUpdated { .. } => {}
            CLIAgentSessionsModelEvent::StatusChanged {
                terminal_view_id,
                agent,
                status,
                session_context,
            } => match status {
                // When the agent resumes its work we can assume that the previous notification is stale.
                CLIAgentSessionStatus::InProgress => {
                    self.remove_notification_by_source(
                        NotificationOrigin::CLISession(*terminal_view_id),
                        ctx,
                    );
                }
                CLIAgentSessionStatus::Success => {
                    let _ = (session_context, agent);
                    self.add_notification(
                        NotificationCategory::Complete,
                        NotificationOrigin::CLISession(*terminal_view_id),
                        *terminal_view_id,
                        ctx,
                    );
                }
                CLIAgentSessionStatus::Blocked { message } => {
                    let _ = (session_context, agent, message);
                    self.add_notification(
                        NotificationCategory::Request,
                        NotificationOrigin::CLISession(*terminal_view_id),
                        *terminal_view_id,
                        ctx,
                    );
                }
            },
        }
    }

    fn handle_history_event(
        &mut self,
        event: &BlocklistAIHistoryEvent,
        ctx: &mut ModelContext<Self>,
    ) {
        // When a conversation is deleted or removed, clean up its notification and pending artifacts.
        if let BlocklistAIHistoryEvent::DeletedConversation {
            conversation_id, ..
        }
        | BlocklistAIHistoryEvent::RemoveConversation {
            conversation_id, ..
        } = event
        {
            self.pending_artifacts.remove(conversation_id);
            self.remove_notification_by_source(
                NotificationOrigin::Conversation(*conversation_id),
                ctx,
            );
            return;
        }

        // Accumulate artifacts as they arrive during the conversation.
        if let BlocklistAIHistoryEvent::UpdatedConversationArtifacts {
            conversation_id,
            artifact,
            ..
        } = event
        {
            self.pending_artifacts
                .entry(*conversation_id)
                .or_default()
                .push(artifact.clone());
            return;
        }

        let BlocklistAIHistoryEvent::UpdatedConversationStatus {
            terminal_view_id,
            conversation_id,
            // We shouldn't trigger toasts when restoring conversations on startup.
            is_restored: false,
        } = event
        else {
            return;
        };

        let ai_history_model = BlocklistAIHistoryModel::as_ref(ctx);
        let Some(updated_conversation) = ai_history_model.conversation(conversation_id) else {
            return;
        };

        if updated_conversation.should_exclude_from_navigation() {
            return;
        }

        let status = updated_conversation.status().clone();
        if !status.should_trigger_notification() {
            return;
        }

        if is_terminal_view_visible(*terminal_view_id, ctx) {
            return;
        }

        let Some((window_id, tab_index)) =
            window_and_tab_idx_id_for_conversation(*conversation_id, ctx)
        else {
            return;
        };

        ctx.emit(AgentManagementEvent::ConversationNeedsAttention {
            window_id,
            tab_index,
            terminal_view_id: *terminal_view_id,
            conversation_id: *conversation_id,
        });
    }

    /// Removes the existing notification for the given source (if any) and emits an update event.
    fn remove_notification_by_source(
        &mut self,
        origin: NotificationOrigin,
        ctx: &mut ModelContext<Self>,
    ) {
        if self.notifications.remove_by_origin(origin) {
            ctx.emit(AgentManagementEvent::NotificationUpdated);
        }
    }

    /// Drains and returns the pending artifacts for a conversation.
    #[cfg(test)]
    pub(crate) fn flush_pending_artifacts(
        &mut self,
        conversation_id: crate::ai::agent::conversation::AIConversationId,
    ) -> Vec<crate::ai::artifacts::Artifact> {
        self.pending_artifacts
            .remove(&conversation_id)
            .unwrap_or_default()
    }

    fn add_notification(
        &mut self,
        category: NotificationCategory,
        origin: NotificationOrigin,
        terminal_view_id: EntityId,
        ctx: &mut ModelContext<Self>,
    ) {
        if !*AISettings::as_ref(ctx).show_agent_notifications {
            return;
        }

        let is_visible = is_terminal_view_visible(terminal_view_id, ctx);
        let item = NotificationItem::new(category, origin, is_visible, terminal_view_id);

        let id = item.id;
        self.notifications.push(item);
        ctx.emit(AgentManagementEvent::NotificationAdded { id });
    }
}

#[derive(Clone, Debug)]
pub enum AgentManagementEvent {
    /// A Warp-native conversation needs attention and is not visible in the current window/tab.
    ConversationNeedsAttention {
        window_id: WindowId,
        tab_index: usize,
        terminal_view_id: EntityId,
        conversation_id: AIConversationId,
    },
    /// A new notification was added to the persistent notification center.
    NotificationAdded { id: NotificationId },
    /// A notification's read state changed.
    NotificationUpdated,
}

impl ConversationStatus {
    /// Returns true if the updating the conversation with this status should trigger some
    /// notification to the user.
    pub fn should_trigger_notification(&self) -> bool {
        matches!(
            self,
            ConversationStatus::Success
                | ConversationStatus::Blocked { .. }
                | ConversationStatus::Error
        )
    }
}

fn is_terminal_view_visible(terminal_view_id: EntityId, app: &AppContext) -> bool {
    let Some(active_id) = active_focused_terminal_id(app) else {
        return false;
    };
    active_id == terminal_view_id
        || is_terminal_view_in_same_tab(&active_id, &terminal_view_id, app)
}

fn window_and_tab_idx_id_for_conversation(
    conversation_id: AIConversationId,
    app: &AppContext,
) -> Option<(WindowId, usize)> {
    WorkspaceRegistry::as_ref(app)
        .all_workspaces(app)
        .iter()
        .find_map(|(window_id, workspace_handle)| {
            workspace_handle
                .as_ref(app)
                .tab_views()
                .enumerate()
                .find_map(|(tab_idx, pane_group)| {
                    pane_group
                        .as_ref(app)
                        .terminal_pane_ids()
                        .filter_map(|pane_id| {
                            pane_group
                                .as_ref(app)
                                .terminal_view_from_pane_id(pane_id, app)
                        })
                        .find_map(|terminal_view| {
                            let terminal_view_conversation_id =
                                terminal_view.as_ref(app).active_conversation_id(app)?;
                            (terminal_view_conversation_id == conversation_id)
                                .then_some((*window_id, tab_idx))
                        })
                })
        })
}

fn active_focused_terminal_id(app: &AppContext) -> Option<EntityId> {
    let active_window = app.windows().active_window()?;
    let workspace = app
        .views_of_type::<Workspace>(active_window)
        .and_then(|views| views.first().cloned())?;

    let workspace = workspace.as_ref(app);
    workspace.active_terminal_id(app)
}

#[cfg(test)]
#[path = "agent_management_model_tests.rs"]
mod tests;
