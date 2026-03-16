use std::path::Path;
use std::path::PathBuf;

use chrono::Utc;
use codex_protocol::ThreadId;
use codex_protocol::config_types::ModeKind;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::SessionSource;
use serde::Deserialize;
use serde::Serialize;
use tracing::warn;

use crate::codex::TurnContext;

const NON_STOP_CHECKPOINTS_DIR: &str = "non-stop-checkpoints";

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
}

pub fn checkpoint_path(codex_home: &Path, thread_id: ThreadId) -> PathBuf {
    codex_home
        .join(NON_STOP_CHECKPOINTS_DIR)
        .join(format!("{thread_id}.json"))
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

    let (status, last_agent_message) = match checkpoint_fields_from_event(event) {
        Some(fields) => fields,
        None => return,
    };
    let existing = read_checkpoint(codex_home, thread_id).await;
    let now = Utc::now().timestamp();
    let checkpoint = NonStopCheckpoint {
        thread_id,
        turn_id: Some(turn_context.sub_id.clone()),
        status,
        collaboration_mode: turn_context.collaboration_mode.mode,
        model: turn_context.collaboration_mode.model().to_string(),
        cwd: turn_context.cwd.clone(),
        session_source: turn_context.session_source.clone(),
        created_at: existing
            .as_ref()
            .map_or(now, |checkpoint| checkpoint.created_at),
        updated_at: now,
        goal_prompt: existing.and_then(|checkpoint| checkpoint.goal_prompt),
        last_agent_message,
    };

    write_checkpoint(codex_home, thread_id, &checkpoint).await;
}

pub async fn register_non_stop_session(
    codex_home: &Path,
    thread_id: ThreadId,
    model: &str,
    cwd: &Path,
    session_source: SessionSource,
    goal_prompt: Option<String>,
) {
    let existing = read_checkpoint(codex_home, thread_id).await;
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
    let existing_last_agent_message = existing.and_then(|checkpoint| checkpoint.last_agent_message);
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
    };

    write_checkpoint(codex_home, thread_id, &checkpoint).await;
}

async fn read_checkpoint(codex_home: &Path, thread_id: ThreadId) -> Option<NonStopCheckpoint> {
    let path = checkpoint_path(codex_home, thread_id);
    let data = tokio::fs::read(&path).await.ok()?;
    serde_json::from_slice(&data).ok()
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

fn checkpoint_fields_from_event(
    event: &EventMsg,
) -> Option<(NonStopCheckpointStatus, Option<String>)> {
    match event {
        EventMsg::TurnStarted(_) => Some((NonStopCheckpointStatus::Running, None)),
        EventMsg::TurnComplete(event) => Some((
            NonStopCheckpointStatus::TurnComplete,
            event.last_agent_message.clone(),
        )),
        EventMsg::TurnAborted(event) => Some((
            NonStopCheckpointStatus::TurnAborted,
            Some(format!("{:?}", event.reason)),
        )),
        EventMsg::Error(event) => {
            Some((NonStopCheckpointStatus::Error, Some(event.message.clone())))
        }
        EventMsg::ShutdownComplete => Some((NonStopCheckpointStatus::Shutdown, None)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_protocol::protocol::TurnAbortReason;
    use codex_protocol::protocol::TurnAbortedEvent;
    use codex_protocol::protocol::TurnCompleteEvent;
    use codex_protocol::protocol::TurnStartedEvent;
    use tempfile::TempDir;

    #[test]
    fn checkpoint_path_uses_expected_directory() {
        let dir = TempDir::new().expect("tempdir");
        let thread_id = ThreadId::new();
        let path = checkpoint_path(dir.path(), thread_id);
        assert!(path.ends_with(format!("non-stop-checkpoints/{thread_id}.json")));
    }

    #[test]
    fn checkpoint_fields_extracts_status_and_message() {
        let complete = EventMsg::TurnComplete(TurnCompleteEvent {
            turn_id: "turn-1".to_string(),
            last_agent_message: Some("done".to_string()),
        });
        assert_eq!(
            checkpoint_fields_from_event(&complete),
            Some((
                NonStopCheckpointStatus::TurnComplete,
                Some("done".to_string())
            ))
        );

        let started = EventMsg::TurnStarted(TurnStartedEvent {
            turn_id: "turn-1".to_string(),
            model_context_window: None,
            collaboration_mode_kind: ModeKind::NonStop,
        });
        assert_eq!(
            checkpoint_fields_from_event(&started),
            Some((NonStopCheckpointStatus::Running, None))
        );

        let aborted = EventMsg::TurnAborted(TurnAbortedEvent {
            turn_id: Some("turn-1".to_string()),
            reason: TurnAbortReason::Interrupted,
        });
        assert_eq!(
            checkpoint_fields_from_event(&aborted),
            Some((
                NonStopCheckpointStatus::TurnAborted,
                Some("Interrupted".to_string())
            ))
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
    }
}
