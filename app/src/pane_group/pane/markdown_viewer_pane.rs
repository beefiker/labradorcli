use std::path::PathBuf;

use labrador_ui::{AppContext, ModelHandle, View, ViewContext, ViewHandle};

use crate::app_state::LeafContents;
use crate::pane_group::pane::markdown_viewer_view::{MarkdownViewerView, MarkdownViewerViewEvent};

use super::{
    view::PaneView, DetachType, PaneConfiguration, PaneContent, PaneGroup, PaneId, ShareableLink,
    ShareableLinkError,
};

/// A pane hosting a [`MarkdownViewerView`] — the read-only, rendered Markdown
/// viewer opened when a Markdown file resolves to `FileTarget::MarkdownViewer`.
pub struct MarkdownViewerPane {
    view: ViewHandle<PaneView<MarkdownViewerView>>,
    pane_configuration: ModelHandle<PaneConfiguration>,
}

impl MarkdownViewerPane {
    pub fn from_view(markdown_view: ViewHandle<MarkdownViewerView>, ctx: &mut AppContext) -> Self {
        let pane_configuration = markdown_view.as_ref(ctx).pane_configuration();

        let view = ctx.add_typed_action_view(markdown_view.window_id(ctx), |ctx| {
            let pane_id = PaneId::from_markdown_viewer_pane_ctx(ctx);
            PaneView::new(pane_id, markdown_view, (), pane_configuration.clone(), ctx)
        });

        Self {
            view,
            pane_configuration,
        }
    }

    pub fn new<V: View>(path: PathBuf, ctx: &mut ViewContext<V>) -> Self {
        let view = ctx.add_typed_action_view(move |ctx| MarkdownViewerView::new(path, ctx));
        Self::from_view(view, ctx)
    }

    pub fn markdown_view(&self, ctx: &AppContext) -> ViewHandle<MarkdownViewerView> {
        self.view.as_ref(ctx).child(ctx)
    }
}

impl PaneContent for MarkdownViewerPane {
    fn id(&self) -> PaneId {
        PaneId::from_markdown_viewer_pane_view(&self.view)
    }

    fn attach(
        &self,
        _group: &PaneGroup,
        focus_handle: crate::pane_group::focus_state::PaneFocusHandle,
        ctx: &mut ViewContext<PaneGroup>,
    ) {
        self.view
            .update(ctx, |view, ctx| view.set_focus_handle(focus_handle, ctx));

        let markdown_view = self.markdown_view(ctx);
        let pane_id = self.id();

        ctx.subscribe_to_view(&markdown_view, move |pane_group, _, event, ctx| {
            let MarkdownViewerViewEvent::Pane(pane_event) = event;
            pane_group.handle_pane_event(pane_id, pane_event, ctx)
        });
        ctx.subscribe_to_view(&self.view, move |group, _, event, ctx| {
            group.handle_pane_view_event(pane_id, event, ctx);
        });
    }

    fn detach(
        &self,
        _group: &PaneGroup,
        _detach_type: DetachType,
        ctx: &mut ViewContext<PaneGroup>,
    ) {
        let markdown_view = self.markdown_view(ctx);
        ctx.unsubscribe_to_view(&markdown_view);
        ctx.unsubscribe_to_view(&self.view);
    }

    fn snapshot(&self, ctx: &AppContext) -> LeafContents {
        LeafContents::MarkdownViewer {
            path: self.markdown_view(ctx).as_ref(ctx).path().to_path_buf(),
        }
    }

    fn has_application_focus(&self, ctx: &mut ViewContext<PaneGroup>) -> bool {
        self.view.is_self_or_child_focused(ctx)
    }

    fn focus(&self, ctx: &mut ViewContext<PaneGroup>) {
        self.markdown_view(ctx)
            .update(ctx, |view, ctx| view.focus(ctx));
    }

    fn shareable_link(
        &self,
        _ctx: &mut ViewContext<PaneGroup>,
    ) -> Result<ShareableLink, ShareableLinkError> {
        Ok(ShareableLink::Base)
    }

    fn pane_configuration(&self) -> ModelHandle<PaneConfiguration> {
        self.pane_configuration.clone()
    }

    fn is_pane_being_dragged(&self, ctx: &AppContext) -> bool {
        self.view.as_ref(ctx).is_being_dragged()
    }
}
