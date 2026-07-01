//! A read-only pane that renders a Markdown file the way Warp does: as a
//! formatted document (headings, bold, lists, tables, code blocks) rather than
//! as raw source.
//!
//! The pane is opened via [`Workspace::open_file_with_target`] when the file
//! target resolves to [`FileTarget::MarkdownViewer`]. It seeds a
//! [`CodeEditorView`] with the file contents at open time and renders it in one
//! of two modes:
//!
//! * [`MarkdownViewMode::Rendered`] — the buffer is initialized from Markdown
//!   (`InitialBufferState::markdown`), so the editor's formatted-text renderer
//!   draws headings, emphasis, tables, etc.
//! * [`MarkdownViewMode::Raw`] — the buffer is initialized as plain text with
//!   Markdown syntax highlighting, showing the underlying source.
//!
//! The pane header exposes a toggle button to switch between the two modes,
//! mirroring Warp's "Rendered / Raw" affordance. The editor is always
//! read-only (selection and copy stay available).

use std::path::{Path, PathBuf};

use labrador_editor::content::buffer::InitialBufferState;
use labrador_editor::render::element::VerticalExpansionBehavior;
use labrador_ui::{
    elements::{ChildView, MouseStateHandle},
    text_layout::ClipConfig,
    ui_components::components::UiComponent,
    AppContext, Element, Entity, ModelHandle, SingletonEntity, TypedActionView, View, ViewContext,
    ViewHandle,
};

use crate::appearance::Appearance;
use crate::code::editor::view::{CodeEditorRenderOptions, CodeEditorView};
use crate::editor::InteractionState;
use crate::pane_group::focus_state::PaneFocusHandle;
use crate::pane_group::{
    pane::view::{self, HeaderContent, StandardHeader, StandardHeaderOptions},
    BackingView, PaneConfiguration, PaneEvent, PaneHeaderAction,
};
use crate::ui_components::blended_colors;
use crate::ui_components::buttons::icon_button_with_color;
use crate::ui_components::icons;

/// How the Markdown file is currently presented to the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkdownViewMode {
    /// Formatted document view (headings, bold, tables, ...).
    Rendered,
    /// Underlying Markdown source with syntax highlighting.
    Raw,
}

impl MarkdownViewMode {
    fn toggled(self) -> Self {
        match self {
            MarkdownViewMode::Rendered => MarkdownViewMode::Raw,
            MarkdownViewMode::Raw => MarkdownViewMode::Rendered,
        }
    }
}

/// Event emitted by the [`MarkdownViewerView`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarkdownViewerViewEvent {
    Pane(PaneEvent),
}

/// Actions supported by the pane header's overflow menu (currently none).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarkdownViewerViewAction {}

/// Custom actions dispatched by elements the view renders inside its pane
/// header (e.g. the rendered/raw toggle button).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarkdownViewerViewCustomAction {
    ToggleMode,
}

/// A pane view backed by a read-only [`CodeEditorView`] that renders a Markdown
/// file either as a formatted document or as raw source.
pub struct MarkdownViewerView {
    editor: ViewHandle<CodeEditorView>,
    path: PathBuf,
    /// The raw file contents, captured once at open time.
    contents: String,
    mode: MarkdownViewMode,
    pane_configuration: ModelHandle<PaneConfiguration>,
    focus_handle: Option<PaneFocusHandle>,
    toggle_button_mouse_state: MouseStateHandle,
}

impl MarkdownViewerView {
    pub fn new(path: PathBuf, ctx: &mut ViewContext<Self>) -> Self {
        let title = Self::title_for_path(&path);
        let pane_configuration = ctx.add_model(|_ctx| PaneConfiguration::new(&title));

        let contents = match std::fs::read_to_string(&path) {
            Ok(contents) => contents,
            Err(err) => {
                log::warn!("Failed to read Markdown file {}: {err:?}", path.display());
                format!("Failed to read file: {err}")
            }
        };

        let mode = MarkdownViewMode::Rendered;

        let editor = ctx.add_typed_action_view(|ctx| {
            let view = CodeEditorView::new(
                None,
                None,
                CodeEditorRenderOptions::new(VerticalExpansionBehavior::FillMaxHeight),
                ctx,
            )
            .with_can_show_diff_ui(false)
            .with_show_line_numbers(false);
            // Read-only pane: disallow editing but keep selection/copy/find.
            view.set_interaction_state(InteractionState::Selectable, ctx);
            view.set_show_current_line_highlights(false, ctx);
            view
        });

        let this = Self {
            editor,
            path,
            contents,
            mode,
            pane_configuration,
            focus_handle: None,
            toggle_button_mouse_state: MouseStateHandle::default(),
        };
        this.apply_mode(ctx);
        this
    }

    pub fn pane_configuration(&self) -> ModelHandle<PaneConfiguration> {
        self.pane_configuration.clone()
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn focus(&mut self, ctx: &mut ViewContext<Self>) {
        ctx.focus(&self.editor);
    }

    fn title_for_path(path: &Path) -> String {
        path.file_name()
            .and_then(|name| name.to_str())
            .map(|name| name.to_string())
            .unwrap_or_else(|| path.display().to_string())
    }

    /// Re-seeds the editor buffer to match `self.mode`.
    fn apply_mode(&self, ctx: &mut ViewContext<Self>) {
        let contents = self.contents.clone();
        let path = self.path.clone();
        let mode = self.mode;
        self.editor.update(ctx, |view, ctx| match mode {
            MarkdownViewMode::Rendered => {
                // Parse the source as Markdown so the editor's formatted-text
                // renderer draws headings/emphasis/tables. Deliberately do not
                // set a code language: syntax highlighting operates on the
                // stripped text and would fight the formatted rendering.
                let state = InitialBufferState::markdown(&contents);
                view.reset(state, ctx);
            }
            MarkdownViewMode::Raw => {
                let state = InitialBufferState::plain_text(&contents);
                view.reset(state, ctx);
                view.set_language_with_path(&path, ctx);
            }
        });
    }

    fn toggle_mode(&mut self, ctx: &mut ViewContext<Self>) {
        self.mode = self.mode.toggled();
        self.apply_mode(ctx);
        ctx.notify();
    }

    /// Renders the header button that toggles between rendered and raw modes.
    fn render_toggle_button(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();
        let ui_builder = appearance.ui_builder().clone();

        // The icon and tooltip describe the mode the button switches *to*.
        let (icon, tooltip) = match self.mode {
            MarkdownViewMode::Rendered => (icons::Icon::Code2, "View raw source"),
            MarkdownViewMode::Raw => (icons::Icon::BookOpen, "View rendered"),
        };
        let tooltip = tooltip.to_string();

        icon_button_with_color(
            appearance,
            icon,
            false, /* active */
            self.toggle_button_mouse_state.clone(),
            blended_colors::text_sub(theme, theme.background()).into(),
        )
        .with_tooltip(move || ui_builder.tool_tip(tooltip.clone()).build().finish())
        .build()
        .on_click(|ctx, _, _| {
            ctx.dispatch_typed_action::<PaneHeaderAction<
                MarkdownViewerViewAction,
                MarkdownViewerViewCustomAction,
            >>(PaneHeaderAction::CustomAction(
                MarkdownViewerViewCustomAction::ToggleMode,
            ));
        })
        .finish()
    }
}

impl Entity for MarkdownViewerView {
    type Event = MarkdownViewerViewEvent;
}

impl View for MarkdownViewerView {
    fn ui_name() -> &'static str {
        "MarkdownViewerView"
    }

    fn render(&self, _app: &AppContext) -> Box<dyn Element> {
        ChildView::new(&self.editor).finish()
    }
}

impl TypedActionView for MarkdownViewerView {
    type Action = MarkdownViewerViewAction;

    fn handle_action(&mut self, _action: &Self::Action, _ctx: &mut ViewContext<Self>) {
        // MarkdownViewerViewAction is currently uninhabited.
    }
}

impl BackingView for MarkdownViewerView {
    type PaneHeaderOverflowMenuAction = MarkdownViewerViewAction;
    type CustomAction = MarkdownViewerViewCustomAction;
    type AssociatedData = ();

    fn handle_pane_header_overflow_menu_action(
        &mut self,
        _action: &Self::PaneHeaderOverflowMenuAction,
        _ctx: &mut ViewContext<Self>,
    ) {
        // No overflow menu items are registered.
    }

    fn handle_custom_action(
        &mut self,
        custom_action: &Self::CustomAction,
        ctx: &mut ViewContext<Self>,
    ) {
        match custom_action {
            MarkdownViewerViewCustomAction::ToggleMode => self.toggle_mode(ctx),
        }
    }

    fn close(&mut self, ctx: &mut ViewContext<Self>) {
        ctx.emit(MarkdownViewerViewEvent::Pane(PaneEvent::Close));
    }

    fn focus_contents(&mut self, ctx: &mut ViewContext<Self>) {
        self.focus(ctx);
    }

    fn render_header_content(
        &self,
        _ctx: &view::HeaderRenderContext<'_>,
        app: &AppContext,
    ) -> HeaderContent {
        HeaderContent::Standard(StandardHeader {
            title: Self::title_for_path(&self.path),
            title_secondary: None,
            title_style: None,
            title_clip_config: ClipConfig::start(),
            title_max_width: None,
            left_of_title: None,
            right_of_title: None,
            left_of_overflow: Some(self.render_toggle_button(app)),
            // Keep the icons always visible so hovering the header doesn't
            // shift the toggle button as the close button appears.
            options: StandardHeaderOptions {
                always_show_icons: true,
                ..StandardHeaderOptions::default()
            },
        })
    }

    fn set_focus_handle(&mut self, focus_handle: PaneFocusHandle, _ctx: &mut ViewContext<Self>) {
        self.focus_handle = Some(focus_handle);
    }
}
