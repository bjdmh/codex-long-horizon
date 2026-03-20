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
use regex_lite::Regex;
use serde::Deserialize;
use serde::Serialize;
use tracing::warn;

use crate::codex::TurnContext;
use crate::stream_events_utils::AWAIT_USER_INPUT_OPEN_TAG;
use crate::stream_events_utils::GOAL_COMPLETE_OPEN_TAG;
use crate::stream_events_utils::TASK_COMPLETE_OPEN_TAG;
use crate::stream_events_utils::raw_assistant_output_text_from_item;

const NON_STOP_CHECKPOINTS_DIR: &str = "non-stop-checkpoints";
const DEFAULT_NON_STOP_BUDGET_SECS: i64 = 48 * 60 * 60;

static CHECKPOINT_CACHE: LazyLock<Mutex<HashMap<ThreadId, NonStopCheckpoint>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static NON_STOP_PROMPT_BUDGET_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    match Regex::new(
        r"(?i)(\d+)\s*(hours?|hrs?|hr|h|days?|d|minutes?|mins?|min|m|小时|时|天|分钟|分)",
    ) {
        Ok(regex) => regex,
        Err(err) => panic!("valid non-stop prompt budget regex: {err}"),
    }
});
static NON_STOP_INNOVATION_CANDIDATE_REGEX: LazyLock<Regex> =
    LazyLock::new(
        || match Regex::new(r"(?s)<innovation_candidate>(.*?)</innovation_candidate>") {
            Ok(regex) => regex,
            Err(err) => panic!("valid innovation candidate regex: {err}"),
        },
    );

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
    GoalComplete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NonStopBudgetSource {
    Default48Hours,
    UserPrompt,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NonStopBudgetWindow {
    pub started_at: i64,
    pub duration_secs: i64,
    pub deadline_at: i64,
    pub source: NonStopBudgetSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NonStopInnovationRisk {
    Unknown,
    Low,
    Medium,
    High,
    Critical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NonStopInnovationStatus {
    Proposed,
    Rejected,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NonStopInnovationTask {
    pub id: String,
    pub title: String,
    pub rationale: Option<String>,
    pub relevance: Option<String>,
    pub estimated_duration_secs: Option<i64>,
    pub risk: NonStopInnovationRisk,
    pub status: NonStopInnovationStatus,
    pub proposed_at: i64,
    pub source_turn_id: Option<String>,
    pub rejection_reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RegisterNonStopSessionOptions {
    pub goal_prompt: Option<String>,
    pub reset_budget_from_user_input: bool,
    pub self_directed_innovation_enabled: bool,
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
    #[serde(default)]
    pub consecutive_task_complete_turns: u32,
    #[serde(default)]
    pub last_user_input_at: Option<i64>,
    #[serde(default)]
    pub budget_window: Option<NonStopBudgetWindow>,
    #[serde(default)]
    pub innovation_backlog: Vec<NonStopInnovationTask>,
    #[serde(default)]
    pub self_directed_innovation_enabled: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct NonStopCheckpointContext {
    pub turn_id: String,
    pub collaboration_mode: ModeKind,
    pub model: String,
    pub cwd: PathBuf,
    pub session_source: SessionSource,
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

impl NonStopCheckpointContext {
    pub(crate) fn from_turn_context(turn_context: &TurnContext) -> Self {
        Self {
            turn_id: turn_context.sub_id.clone(),
            collaboration_mode: turn_context.collaboration_mode.mode,
            model: turn_context.collaboration_mode.model().to_string(),
            cwd: turn_context.cwd.clone(),
            session_source: turn_context.session_source.clone(),
        }
    }
}

async fn read_checkpoint_from_disk(
    codex_home: &Path,
    thread_id: ThreadId,
) -> Option<NonStopCheckpoint> {
    let path = checkpoint_path(codex_home, thread_id);
    tokio::task::spawn_blocking(move || {
        let data = std::fs::read(&path).ok()?;
        serde_json::from_slice(&data).ok()
    })
    .await
    .ok()
    .flatten()
}

pub(crate) async fn maybe_persist_checkpoint(
    codex_home: &Path,
    thread_id: ThreadId,
    turn_context: &TurnContext,
    event: &EventMsg,
) {
    let context = NonStopCheckpointContext::from_turn_context(turn_context);
    maybe_persist_checkpoint_with_context(codex_home, thread_id, &context, event).await;
}

pub(crate) async fn maybe_persist_checkpoint_with_context(
    codex_home: &Path,
    thread_id: ThreadId,
    turn_context: &NonStopCheckpointContext,
    event: &EventMsg,
) {
    if turn_context.collaboration_mode != ModeKind::NonStop {
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
        collaboration_mode: turn_context.collaboration_mode,
        model: turn_context.model.clone(),
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
        consecutive_task_complete_turns: existing
            .as_ref()
            .map_or(0, |checkpoint| checkpoint.consecutive_task_complete_turns),
        last_user_input_at: existing
            .as_ref()
            .and_then(|checkpoint| checkpoint.last_user_input_at),
        budget_window: existing
            .as_ref()
            .and_then(|checkpoint| checkpoint.budget_window.clone()),
        innovation_backlog: existing
            .as_ref()
            .map(|checkpoint| checkpoint.innovation_backlog.clone())
            .unwrap_or_default(),
        self_directed_innovation_enabled: existing
            .as_ref()
            .is_some_and(|checkpoint| checkpoint.self_directed_innovation_enabled),
    };

    match event {
        EventMsg::TurnStarted(_) => {
            checkpoint.turn_id = Some(turn_context.turn_id.clone());
            checkpoint.status = NonStopCheckpointStatus::Running;
            checkpoint.last_agent_message = None;
            checkpoint.last_assistant_control_signal = None;
        }
        EventMsg::RawResponseItem(RawResponseItemEvent { item }) => {
            let Some(signal) = checkpoint_control_signal_from_item(item) else {
                return;
            };
            checkpoint.turn_id = Some(turn_context.turn_id.clone());
            checkpoint.status = NonStopCheckpointStatus::Running;
            checkpoint.last_assistant_control_signal = Some(signal);
            if checkpoint.self_directed_innovation_enabled {
                merge_innovation_candidates_from_item(
                    &mut checkpoint,
                    item,
                    Some(turn_context.turn_id.clone()),
                );
            }
        }
        EventMsg::AgentMessage(event) => {
            checkpoint.turn_id = Some(turn_context.turn_id.clone());
            checkpoint.status = NonStopCheckpointStatus::Running;
            checkpoint.last_agent_message = Some(event.message.clone());
        }
        EventMsg::TurnComplete(event) => {
            checkpoint.turn_id = Some(turn_context.turn_id.clone());
            checkpoint.status = NonStopCheckpointStatus::TurnComplete;
            checkpoint.last_agent_message = event.last_agent_message.clone();
            checkpoint.consecutive_task_complete_turns =
                match checkpoint.last_assistant_control_signal {
                    Some(NonStopCheckpointControlSignal::TaskComplete) => {
                        checkpoint.consecutive_task_complete_turns.saturating_add(1)
                    }
                    Some(
                        NonStopCheckpointControlSignal::Continue
                        | NonStopCheckpointControlSignal::AwaitUserInput
                        | NonStopCheckpointControlSignal::GoalComplete,
                    )
                    | None => 0,
                };
        }
        EventMsg::TurnAborted(event) => {
            checkpoint.turn_id = Some(turn_context.turn_id.clone());
            checkpoint.status = NonStopCheckpointStatus::TurnAborted;
            checkpoint.consecutive_task_complete_turns = 0;
            if checkpoint.last_assistant_control_signal
                != Some(NonStopCheckpointControlSignal::AwaitUserInput)
            {
                checkpoint.last_agent_message = Some(format!("{:?}", event.reason));
            }
        }
        EventMsg::Error(event) => {
            checkpoint.turn_id = Some(turn_context.turn_id.clone());
            checkpoint.status = NonStopCheckpointStatus::Error;
            checkpoint.last_agent_message = Some(event.message.clone());
            checkpoint.consecutive_task_complete_turns = 0;
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
    cache_checkpoint(checkpoint);
    if matches!(
        event,
        EventMsg::TurnComplete(_)
            | EventMsg::TurnAborted(_)
            | EventMsg::Error(_)
            | EventMsg::ShutdownComplete
    ) {
        spawn_checkpoint_write(codex_home.to_path_buf(), thread_id).await;
        return;
    }

    write_checkpoint(codex_home, thread_id).await;
}

pub async fn register_non_stop_session(
    codex_home: &Path,
    thread_id: ThreadId,
    model: &str,
    cwd: &Path,
    session_source: SessionSource,
    options: RegisterNonStopSessionOptions,
) {
    let RegisterNonStopSessionOptions {
        goal_prompt,
        reset_budget_from_user_input,
        self_directed_innovation_enabled,
    } = options;
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
    let existing_last_user_input_at = existing
        .as_ref()
        .and_then(|checkpoint| checkpoint.last_user_input_at);
    let existing_budget_window = existing
        .as_ref()
        .and_then(|checkpoint| checkpoint.budget_window.clone());
    let existing_innovation_backlog = existing
        .as_ref()
        .map(|checkpoint| checkpoint.innovation_backlog.clone())
        .unwrap_or_default();
    let budget_window = if reset_budget_from_user_input {
        Some(non_stop_budget_window_from_prompt(
            goal_prompt.as_deref(),
            now,
        ))
    } else {
        existing_budget_window.or_else(|| {
            Some(non_stop_budget_window_from_prompt(
                goal_prompt.as_deref(),
                now,
            ))
        })
    };
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
        consecutive_task_complete_turns: if reset_budget_from_user_input {
            0
        } else {
            existing
                .as_ref()
                .map_or(0, |checkpoint| checkpoint.consecutive_task_complete_turns)
        },
        last_user_input_at: if reset_budget_from_user_input {
            Some(now)
        } else {
            existing_last_user_input_at
        },
        budget_window,
        innovation_backlog: if reset_budget_from_user_input {
            Vec::new()
        } else {
            existing_innovation_backlog
        },
        self_directed_innovation_enabled: if self_directed_innovation_enabled {
            true
        } else {
            existing
                .as_ref()
                .is_some_and(|checkpoint| checkpoint.self_directed_innovation_enabled)
        },
    };

    cache_checkpoint(checkpoint);
    write_checkpoint(codex_home, thread_id).await;
}

async fn spawn_checkpoint_write(codex_home: PathBuf, thread_id: ThreadId) {
    tokio::spawn(async move {
        write_checkpoint(codex_home.as_path(), thread_id).await;
    });
}

async fn write_checkpoint(codex_home: &Path, thread_id: ThreadId) {
    let path = checkpoint_path(codex_home, thread_id);
    let Some(checkpoint) = cached_checkpoint(thread_id) else {
        return;
    };
    let write_result = tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp_path = path.with_extension("json.tmp");
        let payload = serde_json::to_vec_pretty(&checkpoint).map_err(std::io::Error::other)?;
        std::fs::write(&tmp_path, payload)?;
        std::fs::rename(&tmp_path, &path)?;
        Ok(())
    })
    .await;

    match write_result {
        Ok(Ok(())) => {}
        Ok(Err(err)) => {
            warn!(
                "failed to write non-stop checkpoint {}: {err}",
                checkpoint_path(codex_home, thread_id).display()
            );
        }
        Err(err) => {
            warn!("failed to join non-stop checkpoint writer for {thread_id}: {err}");
        }
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
    if text.contains(GOAL_COMPLETE_OPEN_TAG) {
        return Some(NonStopCheckpointControlSignal::GoalComplete);
    }
    if text.contains(TASK_COMPLETE_OPEN_TAG) {
        return Some(NonStopCheckpointControlSignal::TaskComplete);
    }
    Some(NonStopCheckpointControlSignal::Continue)
}

fn merge_innovation_candidates_from_item(
    checkpoint: &mut NonStopCheckpoint,
    item: &ResponseItem,
    source_turn_id: Option<String>,
) {
    let Some(text) = raw_assistant_output_text_from_item(item) else {
        return;
    };
    for candidate in parse_innovation_candidates(&text, source_turn_id) {
        if checkpoint
            .innovation_backlog
            .iter()
            .any(|existing| existing.id == candidate.id)
        {
            continue;
        }
        checkpoint.innovation_backlog.push(candidate);
    }
}

fn parse_innovation_candidates(
    text: &str,
    source_turn_id: Option<String>,
) -> Vec<NonStopInnovationTask> {
    NON_STOP_INNOVATION_CANDIDATE_REGEX
        .captures_iter(text)
        .filter_map(|captures| captures.get(1).map(|inner| inner.as_str()))
        .filter_map(|payload| parse_innovation_candidate_payload(payload, source_turn_id.clone()))
        .collect()
}

fn parse_innovation_candidate_payload(
    payload: &str,
    source_turn_id: Option<String>,
) -> Option<NonStopInnovationTask> {
    let value = serde_json::from_str::<serde_json::Value>(payload.trim()).ok()?;
    let title = value.get("title")?.as_str()?.trim().to_string();
    if title.is_empty() {
        return None;
    }
    let rationale = value
        .get("rationale")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string);
    let relevance = value
        .get("relevance")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string);
    let estimated_duration_secs = value
        .get("estimated_duration_secs")
        .and_then(serde_json::Value::as_i64)
        .or_else(|| {
            value
                .get("estimated_duration")
                .and_then(serde_json::Value::as_str)
                .and_then(parse_prompt_duration_secs)
        });
    let risk = parse_innovation_risk(
        value
            .get("risk")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default(),
    );
    let proposed_at = Utc::now().timestamp();
    Some(NonStopInnovationTask {
        id: format!(
            "innovation-{proposed_at}-{}",
            sanitize_innovation_id_fragment(&title)
        ),
        title,
        rationale,
        relevance,
        estimated_duration_secs,
        risk,
        status: NonStopInnovationStatus::Proposed,
        proposed_at,
        source_turn_id,
        rejection_reason: None,
    })
}

fn sanitize_innovation_id_fragment(title: &str) -> String {
    let sanitized = title
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect::<String>();
    let compact = sanitized
        .split('-')
        .filter(|part| !part.is_empty())
        .take(6)
        .collect::<Vec<_>>()
        .join("-");
    if compact.is_empty() {
        "candidate".to_string()
    } else {
        compact
    }
}

fn parse_innovation_risk(risk: &str) -> NonStopInnovationRisk {
    match risk.trim().to_ascii_lowercase().as_str() {
        "low" => NonStopInnovationRisk::Low,
        "medium" | "med" => NonStopInnovationRisk::Medium,
        "high" => NonStopInnovationRisk::High,
        "critical" => NonStopInnovationRisk::Critical,
        _ => NonStopInnovationRisk::Unknown,
    }
}

fn non_stop_budget_window_from_prompt(
    goal_prompt: Option<&str>,
    started_at: i64,
) -> NonStopBudgetWindow {
    let parsed_duration = parse_prompt_duration_secs(goal_prompt.unwrap_or_default());
    NonStopBudgetWindow {
        started_at,
        duration_secs: parsed_duration.unwrap_or(DEFAULT_NON_STOP_BUDGET_SECS),
        deadline_at: started_at + parsed_duration.unwrap_or(DEFAULT_NON_STOP_BUDGET_SECS),
        source: if parsed_duration.is_some() {
            NonStopBudgetSource::UserPrompt
        } else {
            NonStopBudgetSource::Default48Hours
        },
    }
}

fn parse_prompt_duration_secs(prompt: &str) -> Option<i64> {
    NON_STOP_PROMPT_BUDGET_REGEX
        .captures_iter(prompt)
        .filter_map(|captures| {
            let amount = captures.get(1)?.as_str().parse::<i64>().ok()?;
            let unit = captures.get(2)?.as_str().to_ascii_lowercase();
            match unit.as_str() {
                "day" | "days" | "d" | "天" => amount.checked_mul(24 * 60 * 60),
                "hour" | "hours" | "hr" | "hrs" | "h" | "小时" | "时" => {
                    amount.checked_mul(60 * 60)
                }
                "minute" | "minutes" | "min" | "mins" | "m" | "分钟" | "分" => {
                    amount.checked_mul(60)
                }
                _ => None,
            }
        })
        .next()
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_protocol::models::ContentItem;
    use codex_protocol::protocol::TurnCompleteEvent;
    use codex_protocol::protocol::TurnCompleteReason;
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

        let goal_complete = ResponseItem::Message {
            id: Some("msg-4".to_string()),
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                text: "<goal_complete>done for real</goal_complete>".to_string(),
            }],
            end_turn: Some(true),
            phase: None,
        };
        assert_eq!(
            checkpoint_control_signal_from_item(&goal_complete),
            Some(NonStopCheckpointControlSignal::GoalComplete)
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

    #[test]
    fn parse_prompt_duration_supports_english_and_chinese_units() {
        assert_eq!(
            parse_prompt_duration_secs("keep going for 3 hours"),
            Some(3 * 60 * 60)
        );
        assert_eq!(parse_prompt_duration_secs("最长48小时"), Some(48 * 60 * 60));
        assert_eq!(parse_prompt_duration_secs("watch for 15m"), Some(15 * 60));
        assert_eq!(parse_prompt_duration_secs("phase 3 only"), None);
    }

    #[test]
    fn parse_innovation_candidate_payload_extracts_structured_task() {
        let candidate = parse_innovation_candidate_payload(
            r#"{
                "title":"Add a flaky-test detector",
                "rationale":"Improve monitoring between explicit user asks",
                "relevance":"Keeps the active goal moving",
                "risk":"medium",
                "estimated_duration":"45m"
            }"#,
            Some("turn-1".to_string()),
        )
        .expect("candidate");

        assert_eq!(candidate.title, "Add a flaky-test detector");
        assert_eq!(candidate.estimated_duration_secs, Some(45 * 60));
        assert_eq!(candidate.risk, NonStopInnovationRisk::Medium);
        assert_eq!(candidate.source_turn_id.as_deref(), Some("turn-1"));
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
            RegisterNonStopSessionOptions {
                goal_prompt: Some("ship the feature".to_string()),
                reset_budget_from_user_input: true,
                self_directed_innovation_enabled: false,
            },
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
        assert_eq!(checkpoint.consecutive_task_complete_turns, 0);
        assert_eq!(checkpoint.last_user_input_at, Some(checkpoint.updated_at));
        assert_eq!(
            checkpoint
                .budget_window
                .as_ref()
                .map(|budget| budget.source),
            Some(NonStopBudgetSource::Default48Hours)
        );
        assert!(checkpoint.innovation_backlog.is_empty());
        assert!(!checkpoint.self_directed_innovation_enabled);
    }

    #[tokio::test]
    async fn register_non_stop_session_resets_budget_and_backlog_on_new_user_input() {
        let dir = TempDir::new().expect("tempdir");
        let thread_id = ThreadId::new();
        register_non_stop_session(
            dir.path(),
            thread_id,
            "gpt-5.4",
            dir.path(),
            SessionSource::Exec,
            RegisterNonStopSessionOptions {
                goal_prompt: Some("watch for 2 hours".to_string()),
                reset_budget_from_user_input: true,
                self_directed_innovation_enabled: true,
            },
        )
        .await;
        let mut checkpoint = read_non_stop_checkpoint(dir.path(), thread_id)
            .await
            .expect("checkpoint");
        checkpoint.innovation_backlog.push(NonStopInnovationTask {
            id: "innovation-1".to_string(),
            title: "Investigate adjacent issue".to_string(),
            rationale: None,
            relevance: None,
            estimated_duration_secs: Some(300),
            risk: NonStopInnovationRisk::Low,
            status: NonStopInnovationStatus::Proposed,
            proposed_at: checkpoint.updated_at,
            source_turn_id: Some("turn-1".to_string()),
            rejection_reason: None,
        });
        cache_checkpoint(checkpoint);
        write_checkpoint(dir.path(), thread_id).await;

        register_non_stop_session(
            dir.path(),
            thread_id,
            "gpt-5.4",
            dir.path(),
            SessionSource::Exec,
            RegisterNonStopSessionOptions {
                goal_prompt: Some("watch for 30 minutes".to_string()),
                reset_budget_from_user_input: true,
                self_directed_innovation_enabled: true,
            },
        )
        .await;

        let updated = read_non_stop_checkpoint(dir.path(), thread_id)
            .await
            .expect("updated checkpoint");
        assert_eq!(updated.innovation_backlog, Vec::new());
        assert_eq!(updated.goal_prompt.as_deref(), Some("watch for 30 minutes"));
        assert_eq!(updated.consecutive_task_complete_turns, 0);
        assert_eq!(
            updated
                .budget_window
                .as_ref()
                .map(|budget| budget.duration_secs),
            Some(30 * 60)
        );
        assert!(updated.self_directed_innovation_enabled);
    }

    #[tokio::test]
    async fn checkpoint_tracks_consecutive_task_complete_turns() {
        let dir = TempDir::new().expect("tempdir");
        let thread_id = ThreadId::new();
        let context = NonStopCheckpointContext {
            turn_id: "turn-1".to_string(),
            collaboration_mode: ModeKind::NonStop,
            model: "gpt-5.4".to_string(),
            cwd: dir.path().to_path_buf(),
            session_source: SessionSource::Exec,
        };

        register_non_stop_session(
            dir.path(),
            thread_id,
            "gpt-5.4",
            dir.path(),
            SessionSource::Exec,
            RegisterNonStopSessionOptions {
                goal_prompt: Some("finish the migration".to_string()),
                reset_budget_from_user_input: true,
                self_directed_innovation_enabled: false,
            },
        )
        .await;

        maybe_persist_checkpoint_with_context(
            dir.path(),
            thread_id,
            &context,
            &EventMsg::RawResponseItem(RawResponseItemEvent {
                item: ResponseItem::Message {
                    id: Some("msg-1".to_string()),
                    role: "assistant".to_string(),
                    content: vec![ContentItem::OutputText {
                        text: "<task_complete>done</task_complete>".to_string(),
                    }],
                    end_turn: Some(true),
                    phase: None,
                },
            }),
        )
        .await;
        maybe_persist_checkpoint_with_context(
            dir.path(),
            thread_id,
            &context,
            &EventMsg::TurnComplete(TurnCompleteEvent {
                turn_id: "turn-1".to_string(),
                last_agent_message: Some("done".to_string()),
                completion_reason: TurnCompleteReason::Completed,
            }),
        )
        .await;

        let first = read_non_stop_checkpoint(dir.path(), thread_id)
            .await
            .expect("first checkpoint");
        assert_eq!(first.consecutive_task_complete_turns, 1);

        maybe_persist_checkpoint_with_context(
            dir.path(),
            thread_id,
            &NonStopCheckpointContext {
                turn_id: "turn-2".to_string(),
                ..context.clone()
            },
            &EventMsg::RawResponseItem(RawResponseItemEvent {
                item: ResponseItem::Message {
                    id: Some("msg-2".to_string()),
                    role: "assistant".to_string(),
                    content: vec![ContentItem::OutputText {
                        text: "<task_complete>done again</task_complete>".to_string(),
                    }],
                    end_turn: Some(true),
                    phase: None,
                },
            }),
        )
        .await;
        maybe_persist_checkpoint_with_context(
            dir.path(),
            thread_id,
            &NonStopCheckpointContext {
                turn_id: "turn-2".to_string(),
                ..context.clone()
            },
            &EventMsg::TurnComplete(TurnCompleteEvent {
                turn_id: "turn-2".to_string(),
                last_agent_message: Some("done again".to_string()),
                completion_reason: TurnCompleteReason::Completed,
            }),
        )
        .await;

        let second = read_non_stop_checkpoint(dir.path(), thread_id)
            .await
            .expect("second checkpoint");
        assert_eq!(second.consecutive_task_complete_turns, 2);

        maybe_persist_checkpoint_with_context(
            dir.path(),
            thread_id,
            &NonStopCheckpointContext {
                turn_id: "turn-3".to_string(),
                ..context
            },
            &EventMsg::RawResponseItem(RawResponseItemEvent {
                item: ResponseItem::Message {
                    id: Some("msg-3".to_string()),
                    role: "assistant".to_string(),
                    content: vec![ContentItem::OutputText {
                        text: "<goal_complete>done for real</goal_complete>".to_string(),
                    }],
                    end_turn: Some(true),
                    phase: None,
                },
            }),
        )
        .await;
        maybe_persist_checkpoint_with_context(
            dir.path(),
            thread_id,
            &NonStopCheckpointContext {
                turn_id: "turn-3".to_string(),
                collaboration_mode: ModeKind::NonStop,
                model: "gpt-5.4".to_string(),
                cwd: dir.path().to_path_buf(),
                session_source: SessionSource::Exec,
            },
            &EventMsg::TurnComplete(TurnCompleteEvent {
                turn_id: "turn-3".to_string(),
                last_agent_message: Some("done for real".to_string()),
                completion_reason: TurnCompleteReason::NoMoreWork,
            }),
        )
        .await;

        let final_checkpoint = read_non_stop_checkpoint(dir.path(), thread_id)
            .await
            .expect("final checkpoint");
        assert_eq!(final_checkpoint.consecutive_task_complete_turns, 0);
    }
}
