use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::sync::LazyLock;
use std::sync::Mutex;

use chrono::Utc;
use codex_protocol::ThreadId;
use codex_protocol::config_types::ModeKind;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::RawResponseItemEvent;
use codex_protocol::protocol::SessionSource;
use serde::Deserialize;
use serde::Serialize;
use tracing::warn;

use crate::codex::TurnContext;
use crate::stream_events_utils::AWAIT_USER_INPUT_OPEN_TAG;
use crate::stream_events_utils::TASK_COMPLETE_OPEN_TAG;
use crate::stream_events_utils::raw_assistant_output_text_from_item;

const NON_STOP_CHECKPOINTS_DIR: &str = "non-stop-checkpoints";
static CHECKPOINT_CACHE: LazyLock<Mutex<HashMap<ThreadId, NonStopCheckpoint>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NonStopCheckpointStatus {
    Pending,
    Running,
    TurnComplete,
    TurnAborted,
    Error,
    Shutdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NonStopCheckpointControlSignal {
    Continue,
    AwaitUserInput,
    TaskComplete,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NonStopCheckpoint {
    pub thread_id: ThreadId,
    pub turn_id: Option<String>,
    pub status: NonStopCheckpointStatus,
    pub collaboration_mode: ModeKind,
    pub model: String,
    pub cwd: PathBuf,
    pub session_source: SessionSource,
    pub created_at: i64,
    pub updated_at: i64,
    pub goal_prompt: Option<String>,
    pub last_agent_message: Option<String>,
    pub last_assistant_control_signal: Option<NonStopCheckpointControlSignal>,
}

pub fn checkpoint_path(codex_home: &Path, thread_id: ThreadId) -> PathBuf {
    codex_home
        .join(NON_STOP_CHECKPOINTS_DIR)
        .join(format!("{thread_id}.json"))
}

pub async fn read_non_stop_checkpoint(
    codex_home: &Path,
    thread_id: ThreadId,
) -> Option<NonStopCheckpoint> {
    if let Some(checkpoint) = cached_checkpoint(thread_id) {
        return Some(checkpoint);
    }
    let checkpoint = read_checkpoint_from_disk(codex_home, thread_id).await?;
    cache_checkpoint(checkpoint.clone());
    Some(checkpoint)
}

async fn read_checkpoint_from_disk(
    codex_home: &Path,
    thread_id: ThreadId,
) -> Option<NonStopCheckpoint> {
    let path = checkpoint_path(codex_home, thread_id);
    let data = tokio::fs::read(&path).await.ok()?;
    serde_json::from_slice(&data).ok()
}

pub(crate) async fn maybe_persist_checkpoint(
    codex_home: &Path,
    thread_id: ThreadId,
    turn_context: &TurnContext,
    event: &EventMsg,
) {
    if turn_context.collaboration_mode.mode != ModeKind::NonStop {
        return;
    }

    let existing = match event {
        EventMsg::TurnStarted(_)
        | EventMsg::RawResponseItem(_)
        | EventMsg::AgentMessage(_)
        | EventMsg::TurnComplete(_)
        | EventMsg::TurnAborted(_)
        | EventMsg::Error(_)
        | EventMsg::ShutdownComplete => read_non_stop_checkpoint(codex_home, thread_id).await,
        _ => return,
    };
    let now = Utc::now().timestamp();
    let mut checkpoint = NonStopCheckpoint {
        thread_id,
        turn_id: existing
            .as_ref()
            .and_then(|checkpoint| checkpoint.turn_id.clone()),
        status: existing
            .as_ref()
            .map_or(NonStopCheckpointStatus::Pending, |checkpoint| {
                checkpoint.status
            }),
        collaboration_mode: turn_context.collaboration_mode.mode,
        model: turn_context.collaboration_mode.model().to_string(),
        cwd: turn_context.cwd.clone(),
        session_source: turn_context.session_source.clone(),
        created_at: existing
            .as_ref()
            .map_or(now, |checkpoint| checkpoint.created_at),
        updated_at: now,
        goal_prompt: existing
            .as_ref()
            .and_then(|checkpoint| checkpoint.goal_prompt.clone()),
        last_agent_message: existing
            .as_ref()
            .and_then(|checkpoint| checkpoint.last_agent_message.clone()),
        last_assistant_control_signal: existing
            .as_ref()
            .and_then(|checkpoint| checkpoint.last_assistant_control_signal),
    };

    match event {
        EventMsg::TurnStarted(_) => {
            checkpoint.turn_id = Some(turn_context.sub_id.clone());
            checkpoint.status = NonStopCheckpointStatus::Running;
            checkpoint.last_agent_message = None;
            checkpoint.last_assistant_control_signal = None;
        }
        EventMsg::RawResponseItem(RawResponseItemEvent { item }) => {
            let Some(signal) = checkpoint_control_signal_from_item(item) else {
                return;
            };
            checkpoint.turn_id = Some(turn_context.sub_id.clone());
            checkpoint.status = NonStopCheckpointStatus::Running;
            checkpoint.last_assistant_control_signal = Some(signal);
        }
        EventMsg::AgentMessage(event) => {
            checkpoint.turn_id = Some(turn_context.sub_id.clone());
            checkpoint.status = NonStopCheckpointStatus::Running;
            checkpoint.last_agent_message = Some(event.message.clone());
        }
        EventMsg::TurnComplete(event) => {
            checkpoint.turn_id = Some(turn_context.sub_id.clone());
            checkpoint.status = NonStopCheckpointStatus::TurnComplete;
            checkpoint.last_agent_message = event.last_agent_message.clone();
        }
        EventMsg::TurnAborted(event) => {
            checkpoint.turn_id = Some(turn_context.sub_id.clone());
            checkpoint.status = NonStopCheckpointStatus::TurnAborted;
            if checkpoint.last_assistant_control_signal
                != Some(NonStopCheckpointControlSignal::AwaitUserInput)
            {
                checkpoint.last_agent_message = Some(format!("{:?}", event.reason));
            }
        }
        EventMsg::Error(event) => {
            checkpoint.turn_id = Some(turn_context.sub_id.clone());
            checkpoint.status = NonStopCheckpointStatus::Error;
            checkpoint.last_agent_message = Some(event.message.clone());
        }
        EventMsg::ShutdownComplete => {
            if !matches!(
                checkpoint.status,
                NonStopCheckpointStatus::TurnComplete
                    | NonStopCheckpointStatus::TurnAborted
                    | NonStopCheckpointStatus::Error
            ) {
                checkpoint.status = NonStopCheckpointStatus::Shutdown;
            }
        }
        _ => return,
    }
    write_checkpoint(codex_home, thread_id, &checkpoint).await;
    cache_checkpoint(checkpoint);
}

pub async fn register_non_stop_session(
    codex_home: &Path,
    thread_id: ThreadId,
    model: &str,
    cwd: &Path,
    session_source: SessionSource,
    goal_prompt: Option<String>,
) {
    let existing = read_non_stop_checkpoint(codex_home, thread_id).await;
    let now = Utc::now().timestamp();
    let existing_turn_id = existing
        .as_ref()
        .and_then(|checkpoint| checkpoint.turn_id.clone());
    let existing_status = existing
        .as_ref()
        .map_or(NonStopCheckpointStatus::Pending, |checkpoint| {
            checkpoint.status
        });
    let existing_created_at = existing
        .as_ref()
        .map_or(now, |checkpoint| checkpoint.created_at);
    let existing_goal_prompt = existing
        .as_ref()
        .and_then(|checkpoint| checkpoint.goal_prompt.clone());
    let existing_last_agent_message = existing
        .as_ref()
        .and_then(|checkpoint| checkpoint.last_agent_message.clone());
    let existing_last_assistant_control_signal = existing
        .as_ref()
        .and_then(|checkpoint| checkpoint.last_assistant_control_signal);
    let checkpoint = NonStopCheckpoint {
        thread_id,
        turn_id: existing_turn_id,
        status: existing_status,
        collaboration_mode: ModeKind::NonStop,
        model: model.to_string(),
        cwd: cwd.to_path_buf(),
        session_source,
        created_at: existing_created_at,
        updated_at: now,
        goal_prompt: goal_prompt.or(existing_goal_prompt),
        last_agent_message: existing_last_agent_message,
        last_assistant_control_signal: existing_last_assistant_control_signal,
    };

    write_checkpoint(codex_home, thread_id, &checkpoint).await;
    cache_checkpoint(checkpoint);
}

async fn write_checkpoint(codex_home: &Path, thread_id: ThreadId, checkpoint: &NonStopCheckpoint) {
    let path = checkpoint_path(codex_home, thread_id);
    if let Some(parent) = path.parent()
        && let Err(err) = tokio::fs::create_dir_all(parent).await
    {
        warn!(
            "failed to create non-stop checkpoint dir {}: {err}",
            parent.display()
        );
        return;
    }
    let tmp_path = path.with_extension("json.tmp");
    let payload = match serde_json::to_vec_pretty(checkpoint) {
        Ok(payload) => payload,
        Err(err) => {
            warn!("failed to serialize non-stop checkpoint for {thread_id}: {err}");
            return;
        }
    };
    if let Err(err) = tokio::fs::write(&tmp_path, payload).await {
        warn!(
            "failed to write non-stop checkpoint temp file {}: {err}",
            tmp_path.display()
        );
        return;
    }
    if let Err(err) = tokio::fs::rename(&tmp_path, &path).await {
        warn!(
            "failed to rename non-stop checkpoint {} -> {}: {err}",
            tmp_path.display(),
            path.display()
        );
    }
}

fn cached_checkpoint(thread_id: ThreadId) -> Option<NonStopCheckpoint> {
    CHECKPOINT_CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&thread_id)
        .cloned()
}

fn cache_checkpoint(checkpoint: NonStopCheckpoint) {
    CHECKPOINT_CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(checkpoint.thread_id, checkpoint);
}

fn checkpoint_control_signal_from_item(
    item: &ResponseItem,
) -> Option<NonStopCheckpointControlSignal> {
    let text = raw_assistant_output_text_from_item(item)?;
    if text.contains(AWAIT_USER_INPUT_OPEN_TAG) {
        return Some(NonStopCheckpointControlSignal::AwaitUserInput);
    }
    if text.contains(TASK_COMPLETE_OPEN_TAG) {
        return Some(NonStopCheckpointControlSignal::TaskComplete);
    }
    Some(NonStopCheckpointControlSignal::Continue)
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_protocol::models::ContentItem;
    use codex_protocol::protocol::RawResponseItemEvent;
    use tempfile::TempDir;

    #[test]
    fn checkpoint_path_uses_expected_directory() {
        let dir = TempDir::new().expect("tempdir");
        let thread_id = ThreadId::new();
        let path = checkpoint_path(dir.path(), thread_id);
        assert!(path.ends_with(format!("non-stop-checkpoints/{thread_id}.json")));
    }

    #[test]
    fn checkpoint_control_signal_extracts_raw_assistant_tags() {
        let blocked = EventMsg::RawResponseItem(RawResponseItemEvent {
            item: ResponseItem::Message {
                id: Some("msg-1".to_string()),
                role: "assistant".to_string(),
                content: vec![ContentItem::OutputText {
                    text: "<await_user_input>I need credentials.</await_user_input>".to_string(),
                }],
                end_turn: Some(true),
                phase: None,
            },
        });
        assert_eq!(
            checkpoint_control_signal_from_item(match blocked {
                EventMsg::RawResponseItem(RawResponseItemEvent { ref item }) => item,
                _ => unreachable!(),
            }),
            Some(NonStopCheckpointControlSignal::AwaitUserInput)
        );

        let subtask_complete = ResponseItem::Message {
            id: Some("msg-2".to_string()),
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                text: "<task_complete>done</task_complete>".to_string(),
            }],
            end_turn: Some(true),
            phase: None,
        };
        assert_eq!(
            checkpoint_control_signal_from_item(&subtask_complete),
            Some(NonStopCheckpointControlSignal::TaskComplete)
        );

        let plain = ResponseItem::Message {
            id: Some("msg-3".to_string()),
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                text: "working".to_string(),
            }],
            end_turn: Some(true),
            phase: None,
        };
        assert_eq!(
            checkpoint_control_signal_from_item(&plain),
            Some(NonStopCheckpointControlSignal::Continue)
        );
    }

    #[tokio::test]
    async fn register_non_stop_session_persists_goal_prompt() {
        let dir = TempDir::new().expect("tempdir");
        let thread_id = ThreadId::new();
        register_non_stop_session(
            dir.path(),
            thread_id,
            "gpt-5.4",
            dir.path(),
            SessionSource::Exec,
            Some("ship the feature".to_string()),
        )
        .await;

        let checkpoint: NonStopCheckpoint = serde_json::from_slice(
            &tokio::fs::read(checkpoint_path(dir.path(), thread_id))
                .await
                .expect("read checkpoint"),
        )
        .expect("parse checkpoint");
        assert_eq!(checkpoint.status, NonStopCheckpointStatus::Pending);
        assert_eq!(checkpoint.goal_prompt.as_deref(), Some("ship the feature"));
        assert_eq!(checkpoint.turn_id, None);
        assert_eq!(checkpoint.last_assistant_control_signal, None);
    }
}
