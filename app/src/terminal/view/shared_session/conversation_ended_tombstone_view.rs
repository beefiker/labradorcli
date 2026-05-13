use warpui::elements::Empty;
use warpui::{AppContext, Element, Entity, EntityId, TypedActionView, View, ViewContext};

/// Tombstone view shown when an agent conversation ends.
/// Stub after removal of ambient agent / cloud features.
pub struct ConversationEndedTombstoneView;

impl ConversationEndedTombstoneView {
    pub fn new(
        _ctx: &mut ViewContext<Self>,
        _terminal_view_id: EntityId,
        _task_id: Option<()>,
    ) -> Self {
        Self
    }
}

#[derive(Clone, Debug)]
pub enum ConversationEndedTombstoneViewEvent {}

impl Entity for ConversationEndedTombstoneView {
    type Event = ConversationEndedTombstoneViewEvent;
}

impl TypedActionView for ConversationEndedTombstoneView {
    type Action = ();
}

impl View for ConversationEndedTombstoneView {
    fn ui_name() -> &'static str {
        "ConversationEndedTombstoneView"
    }

    fn render(&self, _app: &AppContext) -> Box<dyn Element> {
        Empty::new().finish()
    }
}
