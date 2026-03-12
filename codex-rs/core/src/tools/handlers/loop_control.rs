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
use crate::tools::registry::ToolHandler;
use crate::tools::registry::ToolKind;
use crate::tools::spec::JsonSchema;
use codex_protocol::models::FunctionCallOutputBody;

pub static LOOP_CONTROL_TOOL: LazyLock<ToolSpec> = LazyLock::new(|| {
    let mut properties = BTreeMap::new();
    properties.insert(
        "action".to_string(),
        JsonSchema::String {
            description: Some("One of: continue, stop".to_string()),
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

    ToolSpec::Function(ResponsesApiTool {
        name: "loop".to_string(),
        description: "Controls a scheduled long-running loop. During a scheduled wakeup, call this tool to explicitly continue monitoring with a new delay and optional updated objective, or stop monitoring entirely. Prefer this over shell loops, cron, or repeated sleep commands.".to_string(),
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
    Stop,
}

#[derive(Deserialize)]
struct LoopArgs {
    action: LoopAction,
    schedule_id: String,
    next_delay_seconds: Option<u64>,
    next_prompt: Option<String>,
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
                    .update_scheduled_prompt(
                        args.schedule_id.as_str(),
                        args.next_prompt.as_deref(),
                        Some(next_delay_seconds),
                    )
                    .await
                    .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
                if !updated {
                    return Err(FunctionCallError::RespondToModel(format!(
                        "scheduled loop {} was not found or is no longer active",
                        args.schedule_id
                    )));
                }
                format!(
                    "Scheduled loop {} to continue in {} seconds",
                    args.schedule_id, next_delay_seconds
                )
            }
            LoopAction::Stop => {
                let cancelled = state_db
                    .cancel_scheduled_prompt(args.schedule_id.as_str())
                    .await
                    .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
                if !cancelled {
                    return Err(FunctionCallError::RespondToModel(format!(
                        "scheduled loop {} was not found or already inactive",
                        args.schedule_id
                    )));
                }
                format!("Stopped scheduled loop {}", args.schedule_id)
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
    use tokio::sync::Mutex;

    #[tokio::test]
    async fn continue_updates_schedule_interval() {
        let (session, turn) = make_session_and_context().await;
        let state_db = session.state_db().expect("state db");
        let schedule = state_db
            .create_scheduled_prompt(&codex_state::ScheduledPromptCreateParams {
                id: "sched-1".to_string(),
                thread_id: session.conversation_id,
                rollout_path: PathBuf::from("/tmp/rollout.jsonl"),
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
            Some("Scheduled loop sched-1 to continue in 15 seconds")
        );
        assert_eq!(success, Some(true));

        let updated = state_db
            .get_scheduled_prompt("sched-1")
            .await
            .expect("get updated schedule")
            .expect("updated schedule exists");
        assert_eq!(updated.interval_seconds, 15);
    }
}
