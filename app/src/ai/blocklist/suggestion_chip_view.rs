//! Stubbed suggestion chip view. The cloud-backed suggested rules and workflows
//! are removed; the public types remain so dependents continue to compile.

use crate::ai::agent::{SuggestedAgentModeWorkflow, SuggestedLoggingId, SuggestedRule};
use crate::server::ids::SyncId;
use crate::view_components::action_button::ActionButtonTheme;
use pathfinder_color::ColorU;
use warp_core::ui::appearance::Appearance;
use warp_core::ui::theme::Fill;
use warpui::{
    elements::Empty, AppContext, Element, Entity, TypedActionView, View, ViewContext,
};

use crate::ui_components::blended_colors;

use super::suggested_agent_mode_workflow_modal::SuggestedAgentModeWorkflowAndId;
use super::suggested_rule_modal::SuggestedRuleAndId;

pub struct SuggestionDismissButtonTheme;

impl ActionButtonTheme for SuggestionDismissButtonTheme {
    fn background(&self, hovered: bool, appearance: &Appearance) -> Option<Fill> {
        if hovered {
            Some(blended_colors::fg_overlay_2(appearance.theme()))
        } else {
            None
        }
    }

    fn text_color(
        &self,
        _hovered: bool,
        _background: Option<Fill>,
        appearance: &Appearance,
    ) -> ColorU {
        appearance
            .theme()
            .sub_text_color(appearance.theme().background())
            .into()
    }
}

#[derive(Debug, Clone)]
pub enum SuggestedChipViewEvent {
    ShowSuggestedRuleDialog { rule_and_id: SuggestedRuleAndId },
    OpenAIFactCollection { sync_id: Option<SyncId> },
    OpenWorkflow { sync_id: SyncId },
    ShowSuggestedAgentModeWorkflowModal { workflow_and_id: SuggestedAgentModeWorkflowAndId },
}

#[derive(Debug, Clone)]
pub enum SuggestedViewAction {
    ChipClicked,
}

pub struct SuggestionChipView {
    logging_id: SuggestedLoggingId,
}

impl SuggestionChipView {
    pub fn new_rule_chip(rule: SuggestedRule, _ctx: &mut ViewContext<Self>) -> Self {
        Self {
            logging_id: rule.logging_id,
        }
    }

    pub fn new_agent_mode_workflow_chip(
        workflow: SuggestedAgentModeWorkflow,
        _ctx: &mut ViewContext<Self>,
    ) -> Self {
        Self {
            logging_id: workflow.logging_id,
        }
    }

    pub fn logging_id(&self) -> SuggestedLoggingId {
        self.logging_id.clone()
    }
}

impl Entity for SuggestionChipView {
    type Event = SuggestedChipViewEvent;
}

impl View for SuggestionChipView {
    fn ui_name() -> &'static str {
        "SuggestionChipView"
    }

    fn render(&self, _app: &AppContext) -> Box<dyn Element> {
        Empty::new().finish()
    }
}

impl TypedActionView for SuggestionChipView {
    type Action = SuggestedViewAction;

    fn handle_action(&mut self, _action: &Self::Action, _ctx: &mut ViewContext<Self>) {}
}
