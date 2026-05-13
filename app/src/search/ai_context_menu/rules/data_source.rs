use crate::search::ai_context_menu::mixer::AIContextMenuSearchableAction;
use crate::search::data_source::{Query, QueryResult};
use crate::search::mixer::{DataSourceRunErrorWrapper, SyncDataSource};
use warpui::{AppContext, Entity};

pub struct RulesDataSource;

impl RulesDataSource {
    pub fn new() -> Self {
        Self
    }
}

impl SyncDataSource for RulesDataSource {
    type Action = AIContextMenuSearchableAction;

    fn run_query(
        &self,
        _query: &Query,
        _app: &AppContext,
    ) -> Result<Vec<QueryResult<Self::Action>>, DataSourceRunErrorWrapper> {
        Ok(Vec::new())
    }
}

impl Entity for RulesDataSource {
    type Event = ();
}
