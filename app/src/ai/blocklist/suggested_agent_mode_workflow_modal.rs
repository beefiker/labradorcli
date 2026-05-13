//! Stubbed suggested agent mode workflow modal. The workflows subsystem
//! has been removed; the public types remain so dependents continue to compile.

use crate::ai::agent::SuggestedAgentModeWorkflow;
use crate::server::ids::SyncId;
use warpui::{
    elements::Empty, AppContext, Element, Entity, TypedActionView, View, ViewContext,
};

#[derive(Debug, Clone, Default)]
pub struct SuggestedAgentModeWorkflowModal;

#[derive(Debug, Clone)]
pub struct SuggestedAgentModeWorkflowAndId {
    pub workflow: SuggestedAgentModeWorkflow,
    pub sync_id: SyncId,
}

#[derive(Debug, Clone)]
pub enum SuggestedAgentModeWorkflowModalAction {
    /// Triggered when the modal should be cancelled/closed
    Cancel,
}

#[derive(Debug, Clone)]
pub enum SuggestedAgentModeWorkflowModalEvent {
    /// Emitted when the modal should be closed
    Close,
    /// Emitted when a new workflow is successfully created
    WorkflowCreated,
    /// Emitted when the workflow should be run
    RunWorkflow,
}

pub fn init(_app: &mut AppContext) {}

impl SuggestedAgentModeWorkflowModal {
    pub fn close(&mut self, ctx: &mut ViewContext<Self>) {
        ctx.emit(SuggestedAgentModeWorkflowModalEvent::Close);
    }

    pub fn open_workflow(
        &mut self,
        _workflow_and_id: &SuggestedAgentModeWorkflowAndId,
        _ctx: &mut ViewContext<Self>,
    ) {
    }
}

impl Entity for SuggestedAgentModeWorkflowModal {
    type Event = SuggestedAgentModeWorkflowModalEvent;
}

impl View for SuggestedAgentModeWorkflowModal {
    fn ui_name() -> &'static str {
        "SuggestedAgentModeWorkflowModal"
    }

    fn render(&self, _app: &AppContext) -> Box<dyn Element> {
        Empty::new().finish()
    }
}

impl TypedActionView for SuggestedAgentModeWorkflowModal {
    type Action = SuggestedAgentModeWorkflowModalAction;

    fn handle_action(&mut self, _action: &Self::Action, _ctx: &mut ViewContext<Self>) {}
}
