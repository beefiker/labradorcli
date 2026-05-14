//! Stubbed rule editor view.

use crate::server::ids::SyncId;
use warpui::elements::Empty;
use warpui::{AppContext, Element, Entity, TypedActionView, View, ViewContext};

#[derive(Debug, Clone)]
pub enum RuleEditorViewEvent {}

#[derive(Debug, Clone)]
pub enum RuleEditorViewAction {}

pub struct RuleEditorView;

impl RuleEditorView {
    pub fn new(_ctx: &mut ViewContext<Self>) -> Self {
        Self
    }

    pub fn set_ai_rule(&mut self, _sync_id: Option<SyncId>, _ctx: &mut ViewContext<Self>) {}
}

impl Entity for RuleEditorView {
    type Event = RuleEditorViewEvent;
}

impl View for RuleEditorView {
    fn ui_name() -> &'static str {
        "RuleEditorView"
    }

    fn render(&self, _app: &AppContext) -> Box<dyn Element> {
        Empty::new().finish()
    }
}

impl TypedActionView for RuleEditorView {
    type Action = RuleEditorViewAction;

    fn handle_action(&mut self, _action: &Self::Action, _ctx: &mut ViewContext<Self>) {}
}
