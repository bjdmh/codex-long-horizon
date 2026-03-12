use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use codex_protocol::ThreadId;
use codex_protocol::protocol::AgentStatus;
use codex_protocol::protocol::Op;
use codex_protocol::user_input::UserInput;
use tokio::time::MissedTickBehavior;
use tokio::time::timeout;
use tracing::info;
use tracing::warn;
use uuid::Uuid;

use crate::AuthManager;
use crate::ThreadConfigSnapshot;
use crate::ThreadManager;
use crate::config::Config;
use crate::find_thread_path_by_id_str;
use crate::read_session_meta_line;

pub use codex_state::ScheduledPrompt;
pub use codex_state::ScheduledPromptKind;
pub use codex_state::ScheduledPromptStatus;

const SCHEDULED_PROMPT_TICK: Duration = Duration::from_secs(5);
const SCHEDULED_PROMPT_LEASE_SECONDS: i64 = 60;
const SCHEDULED_PROMPT_CLAIM_LIMIT: usize = 8;
const SCHEDULED_PROMPT_RUN_TIMEOUT: Duration = Duration::from_secs(60 * 30);
const SCHEDULED_TASK_RETRY_DELAY: Duration = Duration::from_secs(60 * 5);

#[derive(Clone)]
pub struct ScheduledPromptRuntime {
    state_db: Arc<codex_state::StateRuntime>,
    thread_manager: Arc<ThreadManager>,
    auth_manager: Arc<AuthManager>,
    base_config: Arc<Config>,
    worker_id: String,
}

impl ScheduledPromptRuntime {
    pub async fn new(
        sqlite_home: PathBuf,
        default_provider: String,
        thread_manager: Arc<ThreadManager>,
        auth_manager: Arc<AuthManager>,
        base_config: Config,
    ) -> anyhow::Result<Arc<Self>> {
        let state_db = codex_state::StateRuntime::init(sqlite_home, default_provider, None).await?;
        Ok(Arc::new(Self {
            state_db,
            thread_manager,
            auth_manager,
            base_config: Arc::new(base_config),
            worker_id: format!("scheduled-prompts-{}", Uuid::new_v4()),
        }))
    }

    pub fn start(self: &Arc<Self>) {
        let runtime = Arc::clone(self);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(SCHEDULED_PROMPT_TICK);
            interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                if let Err(err) = runtime.run_due_once().await {
                    warn!("scheduled prompt worker tick failed: {err}");
                }
            }
        });
    }

    pub async fn create_for_thread(
        &self,
        thread_id: ThreadId,
        every: Duration,
        prompt: String,
    ) -> anyhow::Result<ScheduledPrompt> {
        let rollout_path = self.resolve_rollout_path(thread_id).await?.ok_or_else(|| {
            anyhow::anyhow!("thread {thread_id} does not have a persistent rollout")
        })?;
        self.create_for_rollout(thread_id, rollout_path, every, prompt)
            .await
    }

    pub async fn create_task_for_thread(
        &self,
        thread_id: ThreadId,
        run_at: chrono::DateTime<Utc>,
        prompt: String,
    ) -> anyhow::Result<ScheduledPrompt> {
        let rollout_path = self.resolve_rollout_path(thread_id).await?.ok_or_else(|| {
            anyhow::anyhow!("thread {thread_id} does not have a persistent rollout")
        })?;
        self.create_task_for_rollout(thread_id, rollout_path, run_at, prompt)
            .await
    }

    pub async fn create_for_rollout(
        &self,
        thread_id: ThreadId,
        rollout_path: PathBuf,
        every: Duration,
        prompt: String,
    ) -> anyhow::Result<ScheduledPrompt> {
        if prompt.trim().is_empty() {
            return Err(anyhow::anyhow!("scheduled prompt cannot be empty"));
        }
        if every.is_zero() {
            return Err(anyhow::anyhow!(
                "scheduled prompt interval must be greater than zero"
            ));
        }
        let next_run_at = Utc::now()
            + chrono::Duration::from_std(every)
                .map_err(|err| anyhow::anyhow!("invalid schedule interval: {err}"))?;
        let interval_seconds = every.as_secs();
        let params = codex_state::ScheduledPromptCreateParams {
            id: Uuid::new_v4().to_string(),
            thread_id,
            rollout_path,
            kind: codex_state::ScheduledPromptKind::Loop,
            prompt,
            interval_seconds,
            next_run_at,
        };
        self.state_db.create_scheduled_prompt(&params).await
    }

    pub async fn create_task_for_rollout(
        &self,
        thread_id: ThreadId,
        rollout_path: PathBuf,
        run_at: chrono::DateTime<Utc>,
        prompt: String,
    ) -> anyhow::Result<ScheduledPrompt> {
        if prompt.trim().is_empty() {
            return Err(anyhow::anyhow!("scheduled task cannot be empty"));
        }
        let params = codex_state::ScheduledPromptCreateParams {
            id: Uuid::new_v4().to_string(),
            thread_id,
            rollout_path,
            kind: codex_state::ScheduledPromptKind::Task,
            prompt,
            interval_seconds: 0,
            next_run_at: run_at,
        };
        self.state_db.create_scheduled_prompt(&params).await
    }

    pub async fn list(&self, thread_id: Option<&ThreadId>) -> anyhow::Result<Vec<ScheduledPrompt>> {
        self.state_db.list_scheduled_prompts(thread_id).await
    }

    pub async fn cancel(&self, id: &str) -> anyhow::Result<bool> {
        self.state_db.cancel_scheduled_prompt(id).await
    }

    pub async fn resolve_rollout_path(
        &self,
        thread_id: ThreadId,
    ) -> anyhow::Result<Option<PathBuf>> {
        if let Ok(thread) = self.thread_manager.get_thread(thread_id).await
            && let Some(rollout_path) = thread.rollout_path()
        {
            return Ok(Some(rollout_path));
        }
        find_thread_path_by_id_str(
            self.base_config.codex_home.as_path(),
            &thread_id.to_string(),
        )
        .await
        .map_err(Into::into)
    }

    async fn run_due_once(&self) -> anyhow::Result<()> {
        let jobs = self
            .state_db
            .claim_due_scheduled_prompts(
                self.worker_id.as_str(),
                SCHEDULED_PROMPT_LEASE_SECONDS,
                SCHEDULED_PROMPT_CLAIM_LIMIT,
            )
            .await?;
        for job in jobs {
            if let Err(err) = self.execute_job(job).await {
                warn!("scheduled prompt execution failed: {err}");
            }
        }
        Ok(())
    }

    async fn execute_job(&self, job: ScheduledPrompt) -> anyhow::Result<()> {
        let live_thread = self.thread_manager.get_thread(job.thread_id).await.ok();
        let was_loaded = live_thread.is_some();
        let thread = match live_thread {
            Some(thread) => thread,
            None => {
                let rollout_path = if tokio::fs::try_exists(&job.rollout_path)
                    .await
                    .unwrap_or(false)
                {
                    job.rollout_path.clone()
                } else {
                    return self
                        .finish_job_with_error(
                            &job,
                            Some("rollout path no longer exists".to_string()),
                        )
                        .await;
                };
                let mut config = (*self.base_config).clone();
                if let Ok(meta_line) = read_session_meta_line(&rollout_path).await {
                    config.cwd = meta_line.meta.cwd;
                }
                self.thread_manager
                    .resume_thread_from_rollout(
                        config,
                        rollout_path,
                        Arc::clone(&self.auth_manager),
                    )
                    .await?
                    .thread
            }
        };

        let current_status = thread.agent_status().await;
        if current_status == AgentStatus::Running {
            return self
                .finish_job_with_error(
                    &job,
                    Some("thread is busy; skipped this interval".to_string()),
                )
                .await;
        }

        let snapshot = thread.config_snapshot().await;
        let previous_status = current_status;
        let mut status_rx = thread.subscribe_status();
        thread
            .submit(build_scheduled_user_turn(&snapshot, &job))
            .await?;

        let final_status = timeout(SCHEDULED_PROMPT_RUN_TIMEOUT, async {
            let mut observed_new_status = false;
            loop {
                let current = status_rx.borrow().clone();
                if !observed_new_status && current != previous_status {
                    observed_new_status = true;
                }
                if observed_new_status
                    && !matches!(current, AgentStatus::PendingInit | AgentStatus::Running)
                {
                    break current;
                }
                if status_rx.changed().await.is_err() {
                    break AgentStatus::Errored("thread status channel closed".to_string());
                }
            }
        })
        .await
        .unwrap_or_else(|_| AgentStatus::Errored("scheduled run timed out".to_string()));

        let error_message = match final_status {
            AgentStatus::Completed(_) => None,
            AgentStatus::Errored(message) => Some(message),
            AgentStatus::Shutdown => Some("thread shut down during scheduled run".to_string()),
            AgentStatus::NotFound => Some("thread not found during scheduled run".to_string()),
            AgentStatus::PendingInit | AgentStatus::Running => {
                Some("scheduled run ended without a final status".to_string())
            }
        };
        let schedule_after = self.state_db.get_scheduled_prompt(job.id.as_str()).await?;
        let made_loop_decision = schedule_after
            .as_ref()
            .is_some_and(|schedule| schedule.updated_at > job.updated_at);
        let error_message = if error_message.is_none() && !made_loop_decision {
            match job.kind {
                codex_state::ScheduledPromptKind::Loop => {
                    let message =
                        "scheduled run completed without calling the loop tool".to_string();
                    let _ = self.state_db.cancel_scheduled_prompt(job.id.as_str()).await;
                    Some(message)
                }
                codex_state::ScheduledPromptKind::Task => {
                    let _ = self
                        .state_db
                        .complete_scheduled_prompt(job.id.as_str())
                        .await;
                    None
                }
            }
        } else {
            error_message
        };
        self.finish_job_with_error(&job, error_message).await?;

        if !was_loaded {
            let _ = thread.submit(Op::Shutdown {}).await;
            let _ = self.thread_manager.remove_thread(&job.thread_id).await;
        }

        Ok(())
    }

    async fn finish_job_with_error(
        &self,
        job: &ScheduledPrompt,
        error: Option<String>,
    ) -> anyhow::Result<()> {
        let interval_seconds = self
            .state_db
            .get_scheduled_prompt(job.id.as_str())
            .await?
            .map(|schedule| {
                if schedule.interval_seconds == 0 {
                    SCHEDULED_TASK_RETRY_DELAY.as_secs()
                } else {
                    schedule.interval_seconds
                }
            })
            .unwrap_or_else(|| {
                if job.interval_seconds == 0 {
                    SCHEDULED_TASK_RETRY_DELAY.as_secs()
                } else {
                    job.interval_seconds
                }
            });
        let next_run_at = Utc::now()
            + chrono::Duration::seconds(
                i64::try_from(interval_seconds)
                    .map_err(|_| anyhow::anyhow!("invalid job interval"))?,
            );
        let updated = self
            .state_db
            .finish_scheduled_prompt_run(
                job.id.as_str(),
                self.worker_id.as_str(),
                next_run_at,
                error.as_deref(),
            )
            .await?;
        if updated {
            if let Some(error) = error {
                warn!("scheduled prompt {} completed with error: {error}", job.id);
            } else {
                info!("scheduled prompt {} completed successfully", job.id);
            }
        }
        Ok(())
    }
}

fn build_scheduled_user_turn(snapshot: &ThreadConfigSnapshot, job: &ScheduledPrompt) -> Op {
    let prompt = match job.kind {
        codex_state::ScheduledPromptKind::Loop => format!(
            "You are resuming a scheduled long-running loop.\n\n\
Schedule id: {schedule_id}\n\
Schedule kind: loop\n\
Current repeat interval: {interval_seconds} seconds\n\
Completed wakeups so far: {run_count}\n\n\
Current objective/state:\n\
{objective}\n\n\
You must treat this wakeup as a state-machine step.\n\
Before you finish, you must call the `loop` tool exactly once to choose the next state.\n\n\
Allowed choices:\n\
1. Continue monitoring:\n\
   - Call `loop` with action=`continue`\n\
   - Pass this exact schedule_id\n\
   - Set next_delay_seconds to the next wakeup delay\n\
   - Optionally set next_prompt to an updated compact objective/state for the next wakeup\n\
2. Pause monitoring until a later wakeup:\n\
   - Call `loop` with action=`pause`\n\
   - Pass this exact schedule_id\n\
   - Set either resume_delay_seconds or resume_at\n\
   - Optionally set next_prompt to update the stored objective/state\n\
3. Stop monitoring:\n\
   - Call `loop` with action=`stop`\n\
   - Pass this exact schedule_id\n\n\
Important rules:\n\
- Call `loop` exactly once per wakeup.\n\
- Call `loop` before any final free-form conclusion.\n\
- Do not use shell loops, sleep loops, cron, or external timers.\n\
- Prefer bounded observations each wakeup.\n\
- If you continue or pause, keep next_prompt short and operational.\n\
- After the `loop` call, end with a concise final answer wrapped in <task_complete>...</task_complete>.\n\
- Avoid extra commentary before the `loop` call unless it is necessary to justify continue vs pause vs stop.\n\n\
Examples:\n\
- Continue: {{\"action\":\"continue\",\"schedule_id\":\"{schedule_id}\",\"next_delay_seconds\":30,\"next_prompt\":\"Check the latest 200 log lines; stop if no errors for 3 checks.\"}}\n\
- Pause: {{\"action\":\"pause\",\"schedule_id\":\"{schedule_id}\",\"resume_delay_seconds\":1800,\"next_prompt\":\"Deploy is in progress; resume after 30 minutes and verify error rate.\"}}\n\
- Stop: {{\"action\":\"stop\",\"schedule_id\":\"{schedule_id}\"}}\n",
            schedule_id = job.id,
            interval_seconds = job.interval_seconds,
            run_count = job.run_count,
            objective = job.prompt
        ),
        codex_state::ScheduledPromptKind::Task => format!(
            "You are resuming a scheduled future task.\n\n\
Schedule id: {schedule_id}\n\
Schedule kind: task\n\
Completed wakeups so far: {run_count}\n\n\
Task objective:\n\
{objective}\n\n\
Handle the task now.\n\
If the task is fully done after this wakeup, you may finish normally without calling the `loop` tool.\n\
If you need autonomous follow-up monitoring or a deferred retry, you may call the `loop` tool at most once.\n\n\
Optional `loop` choices:\n\
1. Continue monitoring:\n\
   - Call `loop` with action=`continue`\n\
   - Pass this exact schedule_id\n\
   - Set next_delay_seconds to the next wakeup delay\n\
   - Optionally set next_prompt to an updated compact objective/state\n\
   - This converts the task into a recurring loop\n\
2. Pause until a later wakeup:\n\
   - Call `loop` with action=`pause`\n\
   - Pass this exact schedule_id\n\
   - Set either resume_delay_seconds or resume_at\n\
   - Optionally set next_prompt\n\
3. Stop explicitly:\n\
   - Call `loop` with action=`stop`\n\
   - Pass this exact schedule_id\n\n\
Important rules:\n\
- If no `loop` call is needed, just do the task and finish with <task_complete>...</task_complete>.\n\
- If you do call `loop`, call it at most once and before any final free-form conclusion.\n\
- Use `continue` only when you really need a recurring follow-up loop.\n\
- Use `pause` when the task should remain scheduled but sleep until a later time.\n\
- Avoid shell loops, sleep loops, cron, or external timers.\n",
            schedule_id = job.id,
            run_count = job.run_count,
            objective = job.prompt
        ),
    };
    Op::UserTurn {
        items: vec![UserInput::Text {
            text: prompt,
            text_elements: Vec::new(),
        }],
        cwd: snapshot.cwd.clone(),
        approval_policy: snapshot.approval_policy,
        sandbox_policy: snapshot.sandbox_policy.clone(),
        model: snapshot.model.clone(),
        effort: snapshot.reasoning_effort,
        summary: None,
        service_tier: Some(snapshot.service_tier),
        final_output_json_schema: None,
        collaboration_mode: None,
        personality: snapshot.personality,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_protocol::config_types::Personality;
    use codex_protocol::config_types::ServiceTier;
    use codex_protocol::openai_models::ReasoningEffort;
    use codex_protocol::protocol::AskForApproval;
    use codex_protocol::protocol::SandboxPolicy;
    use codex_protocol::protocol::SessionSource;
    use pretty_assertions::assert_eq;
    use std::path::PathBuf;

    #[test]
    fn scheduled_user_turn_uses_thread_snapshot() {
        let snapshot = ThreadConfigSnapshot {
            model: "gpt-5".to_string(),
            model_provider_id: "openai".to_string(),
            service_tier: Some(ServiceTier::Fast),
            approval_policy: AskForApproval::Never,
            sandbox_policy: SandboxPolicy::new_read_only_policy(),
            cwd: PathBuf::from("/tmp/project"),
            ephemeral: false,
            reasoning_effort: Some(ReasoningEffort::Medium),
            personality: Some(Personality::Friendly),
            session_source: SessionSource::Cli,
        };

        let job = ScheduledPrompt {
            id: "sched-1".to_string(),
            thread_id: ThreadId::new(),
            rollout_path: PathBuf::from("/tmp/project/rollout.jsonl"),
            kind: ScheduledPromptKind::Task,
            prompt: "check ci".to_string(),
            interval_seconds: 0,
            next_run_at: Utc::now(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            paused_until: None,
            completed_at: None,
            last_run_started_at: None,
            last_run_completed_at: None,
            last_error: None,
            run_count: 0,
            status: ScheduledPromptStatus::Active,
        };
        let op = build_scheduled_user_turn(&snapshot, &job);
        let Op::UserTurn {
            items,
            cwd,
            approval_policy,
            sandbox_policy,
            model,
            effort,
            service_tier,
            personality,
            ..
        } = op
        else {
            panic!("expected user turn op");
        };

        assert_eq!(items.len(), 1);
        let UserInput::Text {
            text,
            text_elements,
        } = &items[0]
        else {
            panic!("expected text input");
        };
        assert!(text.contains("Schedule id: sched-1"));
        assert!(text.contains("Task objective:\ncheck ci"));
        assert!(text_elements.is_empty());
        assert_eq!(cwd, PathBuf::from("/tmp/project"));
        assert_eq!(approval_policy, AskForApproval::Never);
        assert_eq!(sandbox_policy, SandboxPolicy::new_read_only_policy());
        assert_eq!(model, "gpt-5");
        assert_eq!(effort, Some(ReasoningEffort::Medium));
        assert_eq!(service_tier, Some(Some(ServiceTier::Fast)));
        assert_eq!(personality, Some(Personality::Friendly));
    }
}
