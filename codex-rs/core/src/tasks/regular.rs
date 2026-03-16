use std::sync::Arc;
use std::sync::Mutex;

use crate::client::ModelClient;
use crate::client::ModelClientSession;
use crate::client_common::Prompt;
use crate::codex::TurnContext;
use crate::codex::run_turn;
use crate::error::Result as CodexResult;
use crate::state::TaskKind;
use async_trait::async_trait;
use codex_protocol::user_input::UserInput;
use tokio_util::sync::CancellationToken;
use tracing::Instrument;
use tracing::trace_span;
use tracing::warn;

use super::SessionTask;
use super::SessionTaskContext;

pub(crate) struct RegularTask {
    prewarmed_session: Mutex<Option<ModelClientSession>>,
}

impl Default for RegularTask {
    fn default() -> Self {
        Self {
            prewarmed_session: Mutex::new(None),
        }
    }
}

impl RegularTask {
    pub(crate) async fn with_startup_prewarm(
        model_client: ModelClient,
        prompt: Prompt,
        turn_context: Arc<TurnContext>,
        turn_metadata_header: Option<String>,
    ) -> CodexResult<Self> {
        let mut client_session = model_client.new_session();
        client_session
            .prewarm_websocket(
                &prompt,
                &turn_context.model_info,
                &turn_context.otel_manager,
                turn_context.reasoning_effort,
                turn_context.reasoning_summary,
                turn_context.config.service_tier,
                turn_metadata_header.as_deref(),
            )
            .await?;

        Ok(Self {
            prewarmed_session: Mutex::new(Some(client_session)),
        })
    }

    async fn take_prewarmed_session(&self) -> Option<ModelClientSession> {
        self.prewarmed_session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }
}

#[async_trait]
impl SessionTask for RegularTask {
    fn kind(&self) -> TaskKind {
        TaskKind::Regular
    }

    fn span_name(&self) -> &'static str {
        "session_task.turn"
    }

    async fn run(
        self: Arc<Self>,
        session: Arc<SessionTaskContext>,
        ctx: Arc<TurnContext>,
        input: Vec<UserInput>,
        cancellation_token: CancellationToken,
    ) -> Option<String> {
        let sess = session.clone_session();
        let turn_id = ctx.sub_id.clone();
        let run_turn_span = trace_span!("run_turn");
        sess.set_server_reasoning_included(false).await;
        let prewarmed_client_session = self.take_prewarmed_session().await;
        let result = run_turn(
            sess.clone(),
            Arc::clone(&ctx),
            input,
            prewarmed_client_session,
            cancellation_token.child_token(),
        )
        .instrument(run_turn_span)
        .await;
        warn!(turn_id = %turn_id, "regular task: run_turn returned to RegularTask::run");
        if !cancellation_token.is_cancelled() {
            warn!(turn_id = %turn_id, "regular task: spawning on_task_finished");
            let sess_for_finish = sess.clone();
            let ctx_for_finish = Arc::clone(&ctx);
            let result_for_finish = result.clone();
            tokio::spawn(async move {
                sess_for_finish
                    .on_task_finished(ctx_for_finish, result_for_finish)
                    .await;
            });
        }
        result
    }
}
