use std::collections::BTreeMap;
use std::path::PathBuf;
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

pub static SCHEDULE_TOOL: LazyLock<ToolSpec> = LazyLock::new(|| {
    let mut properties = BTreeMap::new();
    properties.insert(
        "action".to_string(),
        JsonSchema::String {
            description: Some("One of: create_task, create_loop, list, cancel".to_string()),
        },
    );
    properties.insert(
        "prompt".to_string(),
        JsonSchema::String {
            description: Some(
                "Objective to run at wakeup time. Required when action=create_task or action=create_loop."
                    .to_string(),
            ),
        },
    );
    properties.insert(
        "run_at".to_string(),
        JsonSchema::String {
            description: Some(
                "Optional future wakeup time. Supports RFC3339, unix seconds, YYYY-MM-DD HH:MM, today HH:MM, tomorrow HH:MM, or HH:MM in local machine time."
                    .to_string(),
            ),
        },
    );
    properties.insert(
        "delay_seconds".to_string(),
        JsonSchema::Number {
            description: Some(
                "Optional future wakeup delay in seconds. Use instead of run_at for relative scheduling."
                    .to_string(),
            ),
        },
    );
    properties.insert(
        "every_seconds".to_string(),
        JsonSchema::Number {
            description: Some(
                "Repeat interval in seconds. Required when action=create_loop.".to_string(),
            ),
        },
    );
    properties.insert(
        "schedule_id".to_string(),
        JsonSchema::String {
            description: Some("Scheduled item id. Required when action=cancel.".to_string()),
        },
    );

    ToolSpec::Function(ResponsesApiTool {
        name: "schedule".to_string(),
        description: "Creates or manages Codex scheduled wakeups for the current thread. Use create_task for one-time future tasks like 'check this log at 18:00', and create_loop for recurring autonomous monitoring that will later use the loop tool.".to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["action".to_string()]),
            additional_properties: Some(false.into()),
        },
    })
});

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum ScheduleAction {
    CreateTask,
    CreateLoop,
    List,
    Cancel,
}

#[derive(Deserialize)]
struct ScheduleArgs {
    action: ScheduleAction,
    prompt: Option<String>,
    run_at: Option<String>,
    delay_seconds: Option<u64>,
    every_seconds: Option<u64>,
    schedule_id: Option<String>,
}

pub struct ScheduleHandler;

#[async_trait]
impl ToolHandler for ScheduleHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let arguments = match invocation.payload {
            ToolPayload::Function { ref arguments } => arguments,
            _ => {
                return Err(FunctionCallError::RespondToModel(
                    "schedule handler received unsupported payload".to_string(),
                ));
            }
        };

        let args: ScheduleArgs = parse_arguments(arguments.as_str())?;
        let Some(state_db) = invocation.session.state_db() else {
            return Err(FunctionCallError::RespondToModel(
                "scheduled task state is unavailable in this session".to_string(),
            ));
        };

        let message = match args.action {
            ScheduleAction::CreateTask => {
                let prompt = args
                    .prompt
                    .filter(|prompt| !prompt.trim().is_empty())
                    .ok_or_else(|| {
                        FunctionCallError::RespondToModel(
                            "`prompt` is required when action=create_task".to_string(),
                        )
                    })?;
                let rollout_path =
                    resolve_current_rollout_path(&invocation, state_db.as_ref()).await?;
                let run_at = resolve_future_time(args.run_at.as_deref(), args.delay_seconds)
                    .map_err(FunctionCallError::RespondToModel)?;
                let created = state_db
                    .create_scheduled_prompt(&codex_state::ScheduledPromptCreateParams {
                        id: uuid::Uuid::new_v4().to_string(),
                        thread_id: invocation.session.conversation_id,
                        rollout_path,
                        kind: codex_state::ScheduledPromptKind::Task,
                        prompt,
                        interval_seconds: 0,
                        next_run_at: run_at,
                    })
                    .await
                    .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
                format!(
                    "Scheduled task {} for {}",
                    created.id,
                    created.next_run_at.to_rfc3339()
                )
            }
            ScheduleAction::CreateLoop => {
                let prompt = args
                    .prompt
                    .filter(|prompt| !prompt.trim().is_empty())
                    .ok_or_else(|| {
                        FunctionCallError::RespondToModel(
                            "`prompt` is required when action=create_loop".to_string(),
                        )
                    })?;
                let every_seconds = args.every_seconds.ok_or_else(|| {
                    FunctionCallError::RespondToModel(
                        "`every_seconds` is required when action=create_loop".to_string(),
                    )
                })?;
                if every_seconds == 0 {
                    return Err(FunctionCallError::RespondToModel(
                        "`every_seconds` must be greater than zero".to_string(),
                    ));
                }
                let rollout_path =
                    resolve_current_rollout_path(&invocation, state_db.as_ref()).await?;
                let next_run_at = if args.run_at.is_some() || args.delay_seconds.is_some() {
                    resolve_future_time(args.run_at.as_deref(), args.delay_seconds)
                        .map_err(FunctionCallError::RespondToModel)?
                } else {
                    chrono::Utc::now()
                        + chrono::Duration::seconds(i64::try_from(every_seconds).map_err(|_| {
                            FunctionCallError::RespondToModel(
                                "`every_seconds` is too large".to_string(),
                            )
                        })?)
                };
                let created = state_db
                    .create_scheduled_prompt(&codex_state::ScheduledPromptCreateParams {
                        id: uuid::Uuid::new_v4().to_string(),
                        thread_id: invocation.session.conversation_id,
                        rollout_path,
                        kind: codex_state::ScheduledPromptKind::Loop,
                        prompt,
                        interval_seconds: every_seconds,
                        next_run_at,
                    })
                    .await
                    .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
                format!(
                    "Scheduled loop {} to start at {} and repeat every {} seconds",
                    created.id,
                    created.next_run_at.to_rfc3339(),
                    every_seconds
                )
            }
            ScheduleAction::List => {
                let schedules = state_db
                    .list_scheduled_prompts(Some(&invocation.session.conversation_id))
                    .await
                    .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
                if schedules.is_empty() {
                    "No scheduled items for this thread".to_string()
                } else {
                    schedules
                        .into_iter()
                        .map(format_schedule_line)
                        .collect::<Vec<_>>()
                        .join("\n")
                }
            }
            ScheduleAction::Cancel => {
                let schedule_id = args.schedule_id.ok_or_else(|| {
                    FunctionCallError::RespondToModel(
                        "`schedule_id` is required when action=cancel".to_string(),
                    )
                })?;
                let cancelled = state_db
                    .cancel_scheduled_prompt(schedule_id.as_str())
                    .await
                    .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
                if !cancelled {
                    return Err(FunctionCallError::RespondToModel(format!(
                        "scheduled item {schedule_id} was not found or is already inactive"
                    )));
                }
                format!("Cancelled scheduled item {schedule_id}")
            }
        };

        Ok(ToolOutput::Function {
            body: FunctionCallOutputBody::Text(message),
            success: Some(true),
        })
    }
}

async fn resolve_current_rollout_path(
    invocation: &ToolInvocation,
    state_db: &codex_state::StateRuntime,
) -> Result<PathBuf, FunctionCallError> {
    if let Some(rollout) = invocation.session.services.rollout.lock().await.as_ref() {
        return Ok(rollout.rollout_path().to_path_buf());
    }

    if let Some(thread) = state_db
        .get_thread(invocation.session.conversation_id)
        .await
        .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?
    {
        return Ok(thread.rollout_path);
    }

    Err(FunctionCallError::RespondToModel(
        "current thread does not have a persistent rollout yet; create or resume a saved session before scheduling".to_string(),
    ))
}

fn format_schedule_line(schedule: codex_state::ScheduledPrompt) -> String {
    let cadence = if schedule.interval_seconds == 0 {
        "one-shot".to_string()
    } else {
        format!("every {}s", schedule.interval_seconds)
    };
    format!(
        "{} [{}] {} next={} {}",
        schedule.id,
        schedule.status.as_str(),
        schedule.kind.as_str(),
        schedule.next_run_at.to_rfc3339(),
        cadence
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codex::make_session_and_context;
    use crate::tools::context::ToolPayload;
    use crate::tools::handlers::LoopControlHandler;
    use crate::turn_diff_tracker::TurnDiffTracker;
    use codex_protocol::protocol::SessionSource;
    use codex_state::ThreadMetadataBuilder;
    use pretty_assertions::assert_eq;
    use std::sync::Arc;
    use tempfile::tempdir;
    use tokio::sync::Mutex;

    #[tokio::test]
    async fn create_task_schedules_one_off_item() {
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
        let turn = turn
            .with_model("gpt-5.4".to_string(), &session.services.models_manager)
            .await;

        let mut metadata = ThreadMetadataBuilder::new(
            session.conversation_id,
            PathBuf::from("/tmp/rollout.jsonl"),
            chrono::Utc::now(),
            SessionSource::Exec,
        );
        metadata.cwd = PathBuf::from("/tmp");
        let metadata = metadata.build("test-provider");
        state_db
            .upsert_thread(&metadata)
            .await
            .expect("seed thread");

        let output = ScheduleHandler
            .handle(ToolInvocation {
                session: Arc::new(session),
                turn: Arc::new(turn),
                tracker: Arc::new(Mutex::new(TurnDiffTracker::default())),
                call_id: "call-1".to_string(),
                tool_name: "schedule".to_string(),
                payload: ToolPayload::Function {
                    arguments: r#"{"action":"create_task","run_at":"2099-01-02T03:04:05Z","prompt":"check the logs and email if broken"}"#.to_string(),
                },
            })
            .await
            .expect("schedule create task succeeds");

        let ToolOutput::Function { body, success } = output else {
            panic!("expected function output");
        };
        assert_eq!(success, Some(true));
        assert!(
            body.to_text()
                .as_deref()
                .is_some_and(|text| text.contains("Scheduled task"))
        );

        let schedules = state_db
            .list_scheduled_prompts(None)
            .await
            .expect("list scheduled items");
        assert_eq!(schedules.len(), 1);
        assert_eq!(schedules[0].kind, codex_state::ScheduledPromptKind::Task);
        assert_eq!(schedules[0].interval_seconds, 0);
    }

    #[tokio::test]
    async fn scheduled_task_can_pause_continue_and_complete() {
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

        let mut metadata = ThreadMetadataBuilder::new(
            session.conversation_id,
            PathBuf::from("/tmp/rollout.jsonl"),
            chrono::Utc::now(),
            SessionSource::Exec,
        );
        metadata.cwd = PathBuf::from("/tmp");
        let metadata = metadata.build("test-provider");
        state_db
            .upsert_thread(&metadata)
            .await
            .expect("seed thread");

        let tracker = Arc::new(Mutex::new(TurnDiffTracker::default()));
        let session = Arc::new(session);
        let turn = Arc::new(turn);

        ScheduleHandler
            .handle(ToolInvocation {
                session: Arc::clone(&session),
                turn: Arc::clone(&turn),
                tracker: Arc::clone(&tracker),
                call_id: "call-create".to_string(),
                tool_name: "schedule".to_string(),
                payload: ToolPayload::Function {
                    arguments: r#"{"action":"create_task","delay_seconds":120,"prompt":"check logs and follow up if needed"}"#.to_string(),
                },
            })
            .await
            .expect("create task");

        let created = state_db
            .list_scheduled_prompts(None)
            .await
            .expect("list schedules")
            .into_iter()
            .next()
            .expect("created schedule");
        assert_eq!(created.kind, codex_state::ScheduledPromptKind::Task);
        assert_eq!(created.status, codex_state::ScheduledPromptStatus::Active);

        LoopControlHandler
            .handle(ToolInvocation {
                session: Arc::clone(&session),
                turn: Arc::clone(&turn),
                tracker: Arc::clone(&tracker),
                call_id: "call-pause".to_string(),
                tool_name: "loop".to_string(),
                payload: ToolPayload::Function {
                    arguments: format!(
                        r#"{{"action":"pause","schedule_id":"{}","resume_delay_seconds":30,"next_prompt":"wait for deploy then check logs again"}}"#,
                        created.id
                    ),
                },
            })
            .await
            .expect("pause task");

        let paused = state_db
            .get_scheduled_prompt(created.id.as_str())
            .await
            .expect("get paused")
            .expect("paused schedule exists");
        assert_eq!(paused.status, codex_state::ScheduledPromptStatus::Paused);
        assert_eq!(paused.kind, codex_state::ScheduledPromptKind::Task);

        LoopControlHandler
            .handle(ToolInvocation {
                session: Arc::clone(&session),
                turn: Arc::clone(&turn),
                tracker: Arc::clone(&tracker),
                call_id: "call-continue".to_string(),
                tool_name: "loop".to_string(),
                payload: ToolPayload::Function {
                    arguments: format!(
                        r#"{{"action":"continue","schedule_id":"{}","next_delay_seconds":45,"next_prompt":"monitor for recurring failures"}}"#,
                        created.id
                    ),
                },
            })
            .await
            .expect("continue task into loop");

        let continued = state_db
            .get_scheduled_prompt(created.id.as_str())
            .await
            .expect("get continued")
            .expect("continued schedule exists");
        assert_eq!(continued.status, codex_state::ScheduledPromptStatus::Active);
        assert_eq!(continued.kind, codex_state::ScheduledPromptKind::Loop);
        assert_eq!(continued.interval_seconds, 45);

        LoopControlHandler
            .handle(ToolInvocation {
                session,
                turn,
                tracker,
                call_id: "call-stop".to_string(),
                tool_name: "loop".to_string(),
                payload: ToolPayload::Function {
                    arguments: format!(r#"{{"action":"stop","schedule_id":"{}"}}"#, created.id),
                },
            })
            .await
            .expect("complete scheduled item");

        let completed = state_db
            .get_scheduled_prompt(created.id.as_str())
            .await
            .expect("get completed")
            .expect("completed schedule exists");
        assert_eq!(
            completed.status,
            codex_state::ScheduledPromptStatus::Completed
        );
        assert!(completed.completed_at.is_some());
    }
}
