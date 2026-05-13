use warpui::AppContext;

use crate::search::async_snapshot_data_source::AsyncSnapshotDataSource;
use crate::search::data_source::{Query, QueryResult};
use crate::search::mixer::{BoxFuture, DataSourceRunErrorWrapper};

use super::AcceptSlashCommandOrSavedPrompt;

pub(crate) struct SavedPromptsSnapshot;

/// Creates an async data source for saved prompts (Agent Mode workflows) in the slash command
/// menu.
pub(crate) fn saved_prompts_data_source(
) -> AsyncSnapshotDataSource<SavedPromptsSnapshot, AcceptSlashCommandOrSavedPrompt> {
    AsyncSnapshotDataSource::new(
        |_query: &Query, _app: &AppContext| SavedPromptsSnapshot,
        fuzzy_match_saved_prompts,
    )
}

pub(crate) fn fuzzy_match_saved_prompts(
    _snapshot: SavedPromptsSnapshot,
) -> BoxFuture<
    'static,
    Result<Vec<QueryResult<AcceptSlashCommandOrSavedPrompt>>, DataSourceRunErrorWrapper>,
> {
    Box::pin(async move { Ok(Vec::new()) })
}
