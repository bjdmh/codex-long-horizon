use anyhow::Result;
use chrono::DateTime;
use chrono::Utc;
use codex_protocol::ThreadId;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduledPromptStatus {
    Active,
    Cancelled,
}

impl ScheduledPromptStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            ScheduledPromptStatus::Active => "active",
            ScheduledPromptStatus::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ScheduledPrompt {
    pub id: String,
    pub thread_id: ThreadId,
    pub rollout_path: PathBuf,
    pub prompt: String,
    pub interval_seconds: u64,
    pub next_run_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub last_run_started_at: Option<DateTime<Utc>>,
    pub last_run_completed_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub run_count: u64,
    pub status: ScheduledPromptStatus,
}

#[derive(Debug, Clone)]
pub struct ScheduledPromptCreateParams {
    pub id: String,
    pub thread_id: ThreadId,
    pub rollout_path: PathBuf,
    pub prompt: String,
    pub interval_seconds: u64,
    pub next_run_at: DateTime<Utc>,
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct ScheduledPromptRow {
    pub(crate) id: String,
    pub(crate) thread_id: String,
    pub(crate) rollout_path: String,
    pub(crate) prompt: String,
    pub(crate) interval_seconds: i64,
    pub(crate) next_run_at: i64,
    pub(crate) created_at: i64,
    pub(crate) updated_at: i64,
    pub(crate) last_run_started_at: Option<i64>,
    pub(crate) last_run_completed_at: Option<i64>,
    pub(crate) last_error: Option<String>,
    pub(crate) run_count: i64,
    pub(crate) cancelled_at: Option<i64>,
    pub(crate) lease_owner: Option<String>,
    pub(crate) lease_until: Option<i64>,
}

impl TryFrom<ScheduledPromptRow> for ScheduledPrompt {
    type Error = anyhow::Error;

    fn try_from(value: ScheduledPromptRow) -> Result<Self, Self::Error> {
        let status = if value.cancelled_at.is_some() {
            ScheduledPromptStatus::Cancelled
        } else {
            ScheduledPromptStatus::Active
        };
        let interval_seconds = u64::try_from(value.interval_seconds)
            .map_err(|_| anyhow::anyhow!("invalid interval_seconds value"))?;
        let run_count =
            u64::try_from(value.run_count).map_err(|_| anyhow::anyhow!("invalid run_count"))?;
        Ok(Self {
            id: value.id,
            thread_id: ThreadId::from_string(&value.thread_id)?,
            rollout_path: PathBuf::from(value.rollout_path),
            prompt: value.prompt,
            interval_seconds,
            next_run_at: epoch_seconds_to_datetime(value.next_run_at)?,
            created_at: epoch_seconds_to_datetime(value.created_at)?,
            updated_at: epoch_seconds_to_datetime(value.updated_at)?,
            last_run_started_at: value
                .last_run_started_at
                .map(epoch_seconds_to_datetime)
                .transpose()?,
            last_run_completed_at: value
                .last_run_completed_at
                .map(epoch_seconds_to_datetime)
                .transpose()?,
            last_error: value.last_error,
            run_count,
            status,
        })
    }
}

impl ScheduledPromptRow {
    pub(crate) fn is_leased(&self, now: i64) -> bool {
        self.lease_until
            .is_some_and(|lease_until| lease_until >= now)
            && self.lease_owner.is_some()
    }
}

fn epoch_seconds_to_datetime(secs: i64) -> Result<DateTime<Utc>> {
    DateTime::<Utc>::from_timestamp(secs, 0)
        .ok_or_else(|| anyhow::anyhow!("invalid unix timestamp: {secs}"))
}
