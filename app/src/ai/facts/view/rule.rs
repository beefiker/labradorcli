//! Stubbed rule view. The CloudAIFact-backed UI is removed.

use std::path::PathBuf;

use crate::server::ids::SyncId;
use warpui::elements::{Empty, MouseStateHandle};
use warpui::{AppContext, Element, Entity, TypedActionView, View, ViewContext};

pub const HEADER_TEXT: &str = "Rules";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleScope {
    Global,
    ProjectBased,
}

#[derive(Debug, Clone)]
pub enum RuleViewEvent {
    AddRule,
    Edit(SyncId),
    OpenSettings,
    OpenFile(PathBuf),
    InitializeProject(PathBuf),
}

#[derive(Debug, Clone)]
pub enum RuleViewAction {
    AddRule,
    InitializeProject,
    Edit(SyncId),
    OpenSettings,
    SelectScope(RuleScope),
    OpenFile(PathBuf),
}

#[derive(Default, Debug, Clone)]
pub struct MouseStateHandles {
    pub hover: MouseStateHandle,
    pub sync_status_hover: MouseStateHandle,
    pub sync_status_icon: MouseStateHandle,
}

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
