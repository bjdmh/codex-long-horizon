use std::collections::BTreeMap;
use std::sync::LazyLock;

use async_trait::async_trait;
use serde::Deserialize;

use crate::client_common::tools::ResponsesApiTool;
use crate::client_common::tools::ToolSpec;
use crate::function_tool::FunctionCallError;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolOutput;
use crate::tools::context::ToolPayload;
use crate::tools::handlers::parse_arguments;
use crate::tools::handlers::scheduled_time::resolve_future_time;
use crate::tools::registry::ToolHandler;
use crate::tools::registry::ToolKind;
use crate::tools::spec::JsonSchema;
use codex_protocol::models::FunctionCallOutputBody;

pub static LOOP_CONTROL_TOOL: LazyLock<ToolSpec> = LazyLock::new(|| {
    let mut properties = BTreeMap::new();
    properties.insert(
        "action".to_string(),
        JsonSchema::String {
            description: Some("One of: continue, pause, stop".to_string()),
        },
    );
    properties.insert(
        "schedule_id".to_string(),
        JsonSchema::String {
            description: Some("The scheduled loop id from the wakeup prompt".to_string()),
        },
    );
    properties.insert(
        "next_delay_seconds".to_string(),
        JsonSchema::Number {
            description: Some(
                "Delay before the next wakeup in seconds. Required when action=continue"
                    .to_string(),
            ),
        },
    );
    properties.insert(
        "next_prompt".to_string(),
        JsonSchema::String {
            description: Some(
                "Optional updated objective/state for the next wakeup. If omitted, the current objective is reused."
                    .to_string(),
            ),
        },
    );
    properties.insert(
        "resume_delay_seconds".to_string(),
        JsonSchema::Number {
            description: Some(
                "Delay before automatically resuming a paused item in seconds. Required with action=pause unless resume_at is provided."
                    .to_string(),
            ),
        },
    );
    properties.insert(
        "resume_at".to_string(),
        JsonSchema::String {
            description: Some(
                "Absolute future resume time for action=pause. Supports the same formats as the schedule tool run_at field."
                    .to_string(),
            ),
        },
    );

    ToolSpec::Function(ResponsesApiTool {
        name: "loop".to_string(),
        description: "Controls a scheduled Codex loop or deferred task during a scheduled wakeup. Use continue to keep monitoring, pause to sleep until a later automatic resume, or stop to finish the scheduled item. Prefer this over shell loops, cron, or repeated sleep commands.".to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["action".to_string(), "schedule_id".to_string()]),
            additional_properties: Some(false.into()),
        },
    })
});

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum LoopAction {
    Continue,
    Pause,
    Stop,
}

#[derive(Deserialize)]
struct LoopArgs {
    action: LoopAction,
    schedule_id: String,
    next_delay_seconds: Option<u64>,
    next_prompt: Option<String>,
    resume_delay_seconds: Option<u64>,
    resume_at: Option<String>,
}

pub struct LoopControlHandler;

#[async_trait]
impl ToolHandler for LoopControlHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let arguments = match invocation.payload {
            ToolPayload::Function { arguments } => arguments,
            _ => {
                return Err(FunctionCallError::RespondToModel(
                    "loop handler received unsupported payload".to_string(),
                ));
            }
        };

        let args: LoopArgs = parse_arguments(arguments.as_str())?;
        let Some(state_db) = invocation.session.state_db() else {
            return Err(FunctionCallError::RespondToModel(
                "scheduled loop state is unavailable in this session".to_string(),
            ));
        };

        let message = match args.action {
            LoopAction::Continue => {
                let Some(next_delay_seconds) = args.next_delay_seconds else {
                    return Err(FunctionCallError::RespondToModel(
                        "`next_delay_seconds` is required when action=continue".to_string(),
                    ));
                };
                if next_delay_seconds == 0 {
                    return Err(FunctionCallError::RespondToModel(
                        "`next_delay_seconds` must be greater than zero".to_string(),
                    ));
                }
                let updated = state_db
                    .continue_scheduled_prompt(
                        args.schedule_id.as_str(),
                        args.next_prompt.as_deref(),
                        next_delay_seconds,
                    )
                    .await
                    .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
                if !updated {
                    return Err(FunctionCallError::RespondToModel(format!(
                        "scheduled item {} was not found or is no longer active",
                        args.schedule_id
                    )));
                }
                format!(
                    "Scheduled item {} to continue in {} seconds",
                    args.schedule_id, next_delay_seconds
                )
            }
            LoopAction::Pause => {
                let resume_at =
                    resolve_future_time(args.resume_at.as_deref(), args.resume_delay_seconds)
                        .map_err(FunctionCallError::RespondToModel)?;
                let updated = state_db
                    .pause_scheduled_prompt(
                        args.schedule_id.as_str(),
                        args.next_prompt.as_deref(),
                        resume_at,
                    )
                    .await
                    .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
                if !updated {
                    return Err(FunctionCallError::RespondToModel(format!(
                        "scheduled item {} was not found or is no longer active",
                        args.schedule_id
                    )));
                }
                format!(
                    "Paused scheduled item {} until {}",
                    args.schedule_id,
                    resume_at.to_rfc3339()
                )
            }
            LoopAction::Stop => {
                let completed = state_db
                    .complete_scheduled_prompt(args.schedule_id.as_str())
                    .await
                    .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
                if !completed {
                    return Err(FunctionCallError::RespondToModel(format!(
                        "scheduled item {} was not found or already inactive",
                        args.schedule_id
                    )));
                }
                format!("Stopped scheduled item {}", args.schedule_id)
            }
        };

        Ok(ToolOutput::Function {
            body: FunctionCallOutputBody::Text(message),
            success: Some(true),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codex::make_session_and_context;
    use crate::tools::context::ToolPayload;
    use crate::turn_diff_tracker::TurnDiffTracker;
    use pretty_assertions::assert_eq;
    use std::path::PathBuf;
    use std::sync::Arc;
    use tempfile::tempdir;
    use tokio::sync::Mutex;

    #[tokio::test]
    async fn continue_updates_schedule_interval() {
        let temp = tempdir().expect("tempdir");
        let (mut session, turn) = make_session_and_context().await;
        let state_db = codex_state::StateRuntime::init(
            temp.path().to_path_buf(),
            "test-provider".to_string(),
            None,
        )
        .await
        .expect("state db");
        session.services.state_db = Some(Arc::clone(&state_db));
        let state_db = session.state_db().expect("state db");
        let schedule = state_db
            .create_scheduled_prompt(&codex_state::ScheduledPromptCreateParams {
                id: "sched-1".to_string(),
                thread_id: session.conversation_id,
                rollout_path: PathBuf::from("/tmp/rollout.jsonl"),
                kind: codex_state::ScheduledPromptKind::Loop,
                prompt: "watch logs".to_string(),
                interval_seconds: 60,
                next_run_at: chrono::Utc::now(),
            })
            .await
            .expect("create schedule");

        let handler = LoopControlHandler;
        let output = handler
            .handle(ToolInvocation {
                session: Arc::new(session),
                turn: Arc::new(turn),
                tracker: Arc::new(Mutex::new(TurnDiffTracker::default())),
                call_id: "call-1".to_string(),
                tool_name: "loop".to_string(),
                payload: ToolPayload::Function {
                    arguments: format!(
                        r#"{{"action":"continue","schedule_id":"{}","next_delay_seconds":15}}"#,
                        schedule.id
                    ),
                },
            })
            .await
            .expect("loop continue succeeds");

        let ToolOutput::Function { body, success } = output else {
            panic!("expected function output");
        };
        assert_eq!(
            body.to_text().as_deref(),
            Some("Scheduled item sched-1 to continue in 15 seconds")
        );
        assert_eq!(success, Some(true));

        let updated = state_db
            .get_scheduled_prompt("sched-1")
            .await
            .expect("get updated schedule")
            .expect("updated schedule exists");
        assert_eq!(updated.interval_seconds, 15);
    }

    #[tokio::test]
    async fn pause_updates_schedule_resume_time() {
        let temp = tempdir().expect("tempdir");
        let (mut session, turn) = make_session_and_context().await;
        let state_db = codex_state::StateRuntime::init(
            temp.path().to_path_buf(),
            "test-provider".to_string(),
            None,
        )
        .await
        .expect("state db");
        session.services.state_db = Some(Arc::clone(&state_db));
        let state_db = session.state_db().expect("state db");
        state_db
            .create_scheduled_prompt(&codex_state::ScheduledPromptCreateParams {
                id: "sched-2".to_string(),
                thread_id: session.conversation_id,
                rollout_path: PathBuf::from("/tmp/rollout.jsonl"),
                kind: codex_state::ScheduledPromptKind::Loop,
                prompt: "watch logs".to_string(),
                interval_seconds: 60,
                next_run_at: chrono::Utc::now(),
            })
            .await
            .expect("create schedule");

        let output = LoopControlHandler
            .handle(ToolInvocation {
                session: Arc::new(session),
                turn: Arc::new(turn),
                tracker: Arc::new(Mutex::new(TurnDiffTracker::default())),
                call_id: "call-2".to_string(),
                tool_name: "loop".to_string(),
                payload: ToolPayload::Function {
                    arguments:
                        r#"{"action":"pause","schedule_id":"sched-2","resume_delay_seconds":30}"#
                            .to_string(),
                },
            })
            .await
            .expect("loop pause succeeds");

        let ToolOutput::Function { body, success } = output else {
            panic!("expected function output");
        };
        assert_eq!(success, Some(true));
        assert!(
            body.to_text()
                .as_deref()
                .is_some_and(|text| text.contains("Paused scheduled item sched-2 until"))
        );

        let updated = state_db
            .get_scheduled_prompt("sched-2")
            .await
            .expect("get updated schedule")
            .expect("updated schedule exists");
        assert_eq!(updated.status, codex_state::ScheduledPromptStatus::Paused);
        assert!(updated.paused_until.is_some());
    }
}
