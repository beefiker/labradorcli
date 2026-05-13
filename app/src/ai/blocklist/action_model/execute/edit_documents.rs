use futures::{future::BoxFuture, FutureExt};
use warpui::{Entity, ModelContext};

use crate::ai::agent::{
    AIAgentAction, AIAgentActionType, EditDocumentsRequest, EditDocumentsResult,
};

use super::{ActionExecution, AnyActionExecution, ExecuteActionInput, PreprocessActionInput};

pub struct EditDocumentsExecutor;

impl EditDocumentsExecutor {
    pub fn new() -> Self {
        Self
    }

    pub(super) fn should_autoexecute(
        &self,
        _input: ExecuteActionInput,
        _ctx: &mut ModelContext<Self>,
    ) -> bool {
        // Document operations are always auto-executed
        true
    }

    pub(super) fn execute(
        &mut self,
        input: ExecuteActionInput,
        _ctx: &mut ModelContext<Self>,
    ) -> impl Into<AnyActionExecution> {
        let ExecuteActionInput { action, .. } = input;
        let AIAgentAction {
            action: AIAgentActionType::EditDocuments(EditDocumentsRequest { .. }),
            ..
        } = action
        else {
            return ActionExecution::<EditDocumentsResult>::InvalidAction;
        };

        // AI document editing was Notebook-backed; the feature has been removed.
        ActionExecution::Sync(
            EditDocumentsResult::Error("AI document editing is no longer supported".to_string())
                .into(),
        )
    }

    pub(super) fn preprocess_action(
        &mut self,
        _input: PreprocessActionInput,
        _ctx: &mut ModelContext<Self>,
    ) -> BoxFuture<'static, ()> {
        futures::future::ready(()).boxed()
    }
}

impl Entity for EditDocumentsExecutor {
    type Event = ();
}
