use futures::{future::BoxFuture, FutureExt};
use labrador_ui::{Entity, ModelContext, ModelHandle};

use crate::{
    ai::agent::{
        conversation::AIConversationId, AIAgentAction, AIAgentActionType, CreateDocumentsRequest,
        CreateDocumentsResult,
    },
    terminal::model::session::active_session::ActiveSession,
};

use super::{ActionExecution, AnyActionExecution, ExecuteActionInput, PreprocessActionInput};

pub struct CreateDocumentsExecutor {
    _active_session: ModelHandle<ActiveSession>,
    _terminal_view_id: labrador_ui::EntityId,
}

impl CreateDocumentsExecutor {
    pub fn new(
        active_session: ModelHandle<ActiveSession>,
        terminal_view_id: labrador_ui::EntityId,
    ) -> Self {
        Self {
            _active_session: active_session,
            _terminal_view_id: terminal_view_id,
        }
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
        _conversation_id: AIConversationId,
        _ctx: &mut ModelContext<Self>,
    ) -> impl Into<AnyActionExecution> {
        let ExecuteActionInput { action, .. } = input;
        let AIAgentAction {
            action: AIAgentActionType::CreateDocuments(CreateDocumentsRequest { .. }),
            ..
        } = action
        else {
            return ActionExecution::<CreateDocumentsResult>::InvalidAction;
        };

        ActionExecution::Sync(
            CreateDocumentsResult::Success {
                created_documents: Vec::new(),
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

impl Entity for CreateDocumentsExecutor {
    type Event = ();
}
