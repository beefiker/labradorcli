//! Suggested rule modal stub. The Warp Drive-backed UI is removed; we keep the
//! minimal types so that other modules continue to compile.

use crate::ai::agent::SuggestedRule;
use crate::modal::Modal;
use crate::server::ids::SyncId;
use warpui::{
    elements::Empty, AppContext, Element, Entity, SingletonEntity, TypedActionView, View,
    ViewContext, ViewHandle,
};

pub fn init(_app: &mut AppContext) {}

#[derive(Debug, Clone)]
pub enum SuggestedRuleModalEvent {
    AddNewRule { rule: SuggestedRule },
    OpenRuleForEditing { rule: SuggestedRule },
    Close,
}

#[derive(Debug, Clone)]
pub struct SuggestedRuleAndId {
    pub rule: SuggestedRule,
    pub sync_id: SyncId,
}

pub struct SuggestedRuleView;

impl SuggestedRuleView {
    fn new(_ctx: &mut ViewContext<Self>) -> Self {
        Self
    }
}

impl Entity for SuggestedRuleView {
    type Event = ();
}

impl View for SuggestedRuleView {
    fn ui_name() -> &'static str {
        "SuggestedRuleView"
    }

    fn render(&self, _app: &AppContext) -> Box<dyn Element> {
        Empty::new().finish()
    }
}

#[derive(Debug, Clone)]
pub enum SuggestedRuleDialogAction {
    Close,
}

impl TypedActionView for SuggestedRuleView {
    type Action = SuggestedRuleDialogAction;

    fn handle_action(&mut self, _action: &Self::Action, _ctx: &mut ViewContext<Self>) {}
}

pub struct SuggestedRuleModal {
    _modal: ViewHandle<Modal<SuggestedRuleView>>,
}

impl SuggestedRuleModal {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        let view = ctx.add_typed_action_view(SuggestedRuleView::new);
        let modal = ctx.add_view(|ctx| Modal::new(None, view, ctx));
        Self { _modal: modal }
    }

    pub fn set_rule_and_id(
        &mut self,
        _rule_and_id: &SuggestedRuleAndId,
        _ctx: &mut ViewContext<Self>,
    ) {
    }
}

impl Entity for SuggestedRuleModal {
    type Event = SuggestedRuleModalEvent;
}

impl View for SuggestedRuleModal {
    fn ui_name() -> &'static str {
        "SuggestedRuleModal"
    }

    fn render(&self, _app: &AppContext) -> Box<dyn Element> {
        Empty::new().finish()
    }
}

impl TypedActionView for SuggestedRuleModal {
    type Action = SuggestedRuleDialogAction;

    fn handle_action(&mut self, _action: &Self::Action, _ctx: &mut ViewContext<Self>) {}
}
