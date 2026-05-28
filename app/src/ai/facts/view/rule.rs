//! Stubbed rule view. The CloudAIFact-backed UI is removed.

use labrador_ui::elements::Empty;
use labrador_ui::{AppContext, Element, Entity, TypedActionView, View, ViewContext};

pub const HEADER_TEXT: &str = "Rules";

#[derive(Debug, Clone)]
pub enum RuleViewEvent {}

#[derive(Debug, Clone)]
pub enum RuleViewAction {}

pub struct RuleView;

impl RuleView {
    pub fn new(_ctx: &mut ViewContext<Self>) -> Self {
        Self
    }
}

impl Entity for RuleView {
    type Event = RuleViewEvent;
}

impl View for RuleView {
    fn ui_name() -> &'static str {
        "RuleView"
    }

    fn render(&self, _app: &AppContext) -> Box<dyn Element> {
        Empty::new().finish()
    }
}

impl TypedActionView for RuleView {
    type Action = RuleViewAction;

    fn handle_action(&mut self, _action: &Self::Action, _ctx: &mut ViewContext<Self>) {}
}
