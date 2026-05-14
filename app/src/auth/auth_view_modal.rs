//! Stub: Warp Cloud login modal removed.
//!
//! The actual login modal (signup, anonymous user, email auth, etc.) was
//! deleted when dwarf forked from warp. These types remain as inert shells so
//! that consumer code that still references `AuthView`, `AuthViewVariant`, and
//! `AuthRedirectPayload` compiles without needing a wholesale refactor of
//! every login-gated feature.

use serde::{Deserialize, Serialize};
use warpui::{
    elements::Empty, AppContext, Element, Entity, TypedActionView, View, ViewContext,
};

#[derive(Clone, Copy, Debug)]
pub enum AuthViewVariant {
    RequireLoginCloseable,
    ShareRequirementCloseable,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AuthRedirectPayload {
    pub variant_override: Option<String>,
    pub user_id: Option<String>,
    pub token: Option<String>,
    pub refresh_token: Option<String>,
    pub user_uid: Option<String>,
    pub deleted_anonymous_user: Option<String>,
    pub state: Option<String>,
}

#[derive(Clone, Debug)]
pub enum AuthViewEvent {
    Close,
}

#[derive(Clone, Debug)]
pub enum AuthViewAction {
    Close,
}

pub struct AuthView;

impl AuthView {
    pub fn new(_variant: AuthViewVariant, _ctx: &mut ViewContext<Self>) -> Self {
        Self
    }

    pub fn skip_to_browser_open_step(&mut self, _ctx: &mut ViewContext<Self>) {}

    pub fn set_variant(&mut self, _variant: AuthViewVariant, _ctx: &mut ViewContext<Self>) {}
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
