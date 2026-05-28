use futures::{future::BoxFuture, FutureExt};
use labrador_ui::{Entity, ModelContext};

use crate::ai::agent::{
    AIAgentAction, AIAgentActionType, ReadDocumentsRequest, ReadDocumentsResult,
};

use super::{ActionExecution, AnyActionExecution, ExecuteActionInput, PreprocessActionInput};

pub struct ReadDocumentsExecutor;

impl ReadDocumentsExecutor {
    pub fn new() -> Self {
        Self
    }

    pub(super) fn should_autoexecute(
        &self,
        _input: ExecuteActionInput,
        _ctx: &mut ModelContext<Self>,
    ) -> bool {
        true
    }

    pub(super) fn execute(
        &mut self,
        input: ExecuteActionInput,
        _ctx: &mut ModelContext<Self>,
    ) -> impl Into<AnyActionExecution> {
        let ExecuteActionInput { action, .. } = input;
        let AIAgentAction {
            action: AIAgentActionType::ReadDocuments(ReadDocumentsRequest { .. }),
            ..
        } = action
        else {
            return ActionExecution::<ReadDocumentsResult>::InvalidAction;
        };

        ActionExecution::Sync(
            ReadDocumentsResult::Success {
                documents: Vec::new(),
            }
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

impl Entity for ReadDocumentsExecutor {
    type Event = ();
}
