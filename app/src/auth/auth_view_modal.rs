//! Stub: Labrador Cloud login modal removed.
//!
//! The actual login modal (signup, anonymous user, email auth, etc.) was
//! deleted when Labrador forked from the upstream app. These types remain as inert shells so
//! that consumer code that still references `AuthView` and `AuthViewVariant`
//! compiles without needing a wholesale refactor of every login-gated feature.

use labrador_ui::{elements::Empty, AppContext, Element, Entity, TypedActionView, View, ViewContext};

#[derive(Clone, Copy, Debug)]
pub enum AuthViewVariant {
    RequireLoginCloseable,
    ShareRequirementCloseable,
}

#[derive(Clone, Debug)]
pub enum AuthViewEvent {}

#[derive(Clone, Debug)]
pub enum AuthViewAction {}

pub struct AuthView;

impl AuthView {
    pub fn new(_variant: AuthViewVariant, _ctx: &mut ViewContext<Self>) -> Self {
        Self
    }

    pub fn skip_to_browser_open_step(&mut self, _ctx: &mut ViewContext<Self>) {}
}

impl Entity for AuthView {
    type Event = AuthViewEvent;
}

impl View for AuthView {
    fn ui_name() -> &'static str {
        "AuthView"
    }

    fn render(&self, _app: &AppContext) -> Box<dyn Element> {
        Empty::new().finish()
    }
}

impl TypedActionView for AuthView {
    type Action = AuthViewAction;

    fn handle_action(&mut self, _action: &Self::Action, _ctx: &mut ViewContext<Self>) {}
}
