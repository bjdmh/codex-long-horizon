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
    Running,
    TurnComplete,
    TurnAborted,
    Error,
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NonStopCheckpoint {
    pub thread_id: ThreadId,
    pub turn_id: String,
    pub status: NonStopCheckpointStatus,
    pub collaboration_mode: ModeKind,
    pub model: String,
    pub cwd: PathBuf,
    pub session_source: SessionSource,
    pub updated_at: i64,
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
    let checkpoint = NonStopCheckpoint {
        thread_id,
        turn_id: turn_context.sub_id.clone(),
        status,
        collaboration_mode: turn_context.collaboration_mode.mode,
        model: turn_context.collaboration_mode.model().to_string(),
        cwd: turn_context.cwd.clone(),
        session_source: turn_context.session_source.clone(),
        updated_at: Utc::now().timestamp(),
        last_agent_message,
    };

    let path = checkpoint_path(codex_home, thread_id);
    if let Some(parent) = path.parent()
        && let Err(err) = tokio::fs::create_dir_all(parent).await
    {
        warn!("failed to create non-stop checkpoint dir {}: {err}", parent.display());
        return;
    }
    let tmp_path = path.with_extension("json.tmp");
    let payload = match serde_json::to_vec_pretty(&checkpoint) {
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
            Some((NonStopCheckpointStatus::TurnComplete, Some("done".to_string())))
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
}
