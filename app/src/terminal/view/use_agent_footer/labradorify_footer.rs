use std::sync::Arc;

use parking_lot::FairMutex;
use labrador_core::channel::ChannelState;
use labrador_ui::prelude::Empty;
use labrador_ui::{
    elements::{
        ChildView, Container, CrossAxisAlignment, Expanded, Flex, MainAxisSize, ParentElement,
    },
    AppContext, Element, Entity, TypedActionView, View, ViewContext, ViewHandle,
};

use crate::{
    terminal::view::{TerminalModel, PADDING_LEFT},
    ui_components::icons::Icon,
    view_components::action_button::{ActionButton, ButtonSize, KeystrokeSource, TooltipAlignment},
};

use super::{AgentFooterButtonTheme, USE_AGENT_KEYSTROKE};
use crate::terminal::view::block_banner::LabradorificationMode;

/// Footer view rendered for detected subshell/SSH commands, offering both
/// "Labradorify" and "Use agent" buttons in a horizontal row.
pub(super) struct LabradorifyFooterView {
    terminal_model: Arc<FairMutex<TerminalModel>>,
    labradorify_button: ViewHandle<ActionButton>,
    use_agent_button: ViewHandle<ActionButton>,
    dismiss_button: ViewHandle<ActionButton>,
    mode: Option<LabradorificationMode>,
}

impl LabradorifyFooterView {
    pub fn new(terminal_model: Arc<FairMutex<TerminalModel>>, ctx: &mut ViewContext<Self>) -> Self {
        let button_size = ButtonSize::XSmall;

        let labradorify_button = ctx.add_typed_action_view(|_ctx| {
            ActionButton::new(
                format!("{} subshell", ChannelState::app_name_verbify()),
                AgentFooterButtonTheme::new(None),
            )
            .with_icon(Icon::AgentMode)
            .with_size(button_size)
            .with_tooltip(format!(
                "Enable {} shell integration in this session",
                ChannelState::app_name_display()
            ))
            .with_tooltip_alignment(TooltipAlignment::Left)
            .on_click(|ctx| {
                ctx.dispatch_typed_action(LabradorifyFooterViewAction::Labradorify);
            })
        });

        let use_agent_button = ctx.add_typed_action_view(|ctx| {
            ActionButton::new("Use agent", AgentFooterButtonTheme::new(None))
                .with_icon(Icon::AgentMode)
                .with_keybinding(KeystrokeSource::Fixed(USE_AGENT_KEYSTROKE.clone()), ctx)
                .with_size(button_size)
                .with_tooltip(format!(
                    "Ask the {} agent to assist",
                    ChannelState::app_name_display()
                ))
                .with_tooltip_alignment(TooltipAlignment::Left)
                .on_click(|ctx| {
                    ctx.dispatch_typed_action(LabradorifyFooterViewAction::UseAgent);
                })
        });

        let dismiss_button = ctx.add_typed_action_view(|_ctx| {
            ActionButton::new("Dismiss", AgentFooterButtonTheme::new(None))
                .with_size(button_size)
                .on_click(|ctx| {
                    ctx.dispatch_typed_action(LabradorifyFooterViewAction::Dismiss);
                })
        });

        Self {
            terminal_model,
            labradorify_button,
            use_agent_button,
            dismiss_button,
            mode: None,
        }
    }

    /// Updates the Labradorify button label, keybinding, and stores the current mode.
    pub fn set_mode(&mut self, mode: LabradorificationMode, ctx: &mut ViewContext<Self>) {
        let (label, binding_name) = match mode {
            LabradorificationMode::Ssh { .. } => (
                format!("{} SSH session", ChannelState::app_name_verbify()),
                "terminal:labradorify_ssh_session",
            ),
            LabradorificationMode::Subshell { .. } => (
                format!("{} subshell", ChannelState::app_name_verbify()),
                "terminal:labradorify_subshell",
            ),
        };
        self.labradorify_button.update(ctx, |button, ctx| {
            button.set_label(label, ctx);
            button.set_keybinding(Some(KeystrokeSource::Binding(binding_name)), ctx);
        });
        self.mode = Some(mode);
        ctx.notify();
    }

    /// Returns the current Labradorification mode, if set.
    pub fn mode(&self) -> Option<&LabradorificationMode> {
        self.mode.as_ref()
    }

    /// Clears the Labradorification mode.
    pub fn clear_mode(&mut self, ctx: &mut ViewContext<Self>) {
        self.mode = None;
        self.labradorify_button.update(ctx, |button, ctx| {
            button.set_keybinding(None, ctx);
        });
        ctx.notify();
    }
}

#[derive(Debug, Clone)]
pub enum LabradorifyFooterViewAction {
    Labradorify,
    UseAgent,
    Dismiss,
}

pub enum LabradorifyFooterViewEvent {
    Labradorify { mode: LabradorificationMode },
    UseAgent,
    Dismiss,
}

impl Entity for LabradorifyFooterView {
    type Event = LabradorifyFooterViewEvent;
}

impl View for LabradorifyFooterView {
    fn ui_name() -> &'static str {
        "LabradorifyFooterView"
    }

    fn render(&self, _app: &AppContext) -> Box<dyn Element> {
        let terminal_model = self.terminal_model.lock();

        let button_row = Flex::row()
            .with_spacing(4.)
            .with_main_axis_size(MainAxisSize::Max)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_child(ChildView::new(&self.labradorify_button).finish())
            .with_child(ChildView::new(&self.use_agent_button).finish())
            .with_child(Expanded::new(1., Empty::new().finish()).finish())
            .with_child(ChildView::new(&self.dismiss_button).finish());

        let mut container = Container::new(button_row.finish())
            .with_horizontal_padding(*PADDING_LEFT)
            .with_vertical_padding(4.);

        if terminal_model.is_alt_screen_active() {
            if let Some(bg_color) = terminal_model.alt_screen().inferred_bg_color() {
                container = container.with_background(bg_color);
            }
        }

        container.finish()
    }
}

impl TypedActionView for LabradorifyFooterView {
    type Action = LabradorifyFooterViewAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            LabradorifyFooterViewAction::Labradorify => {
                if let Some(mode) = self.mode.clone() {
                    self.clear_mode(ctx);
                    ctx.emit(LabradorifyFooterViewEvent::Labradorify { mode });
                }
            }
            LabradorifyFooterViewAction::UseAgent => {
                self.clear_mode(ctx);
                ctx.emit(LabradorifyFooterViewEvent::UseAgent);
            }
            LabradorifyFooterViewAction::Dismiss => {
                self.clear_mode(ctx);
                ctx.emit(LabradorifyFooterViewEvent::Dismiss);
            }
        }
    }
}
