use super::*;
use crate::ScheduledPrompt;
use crate::ScheduledPromptCreateParams;
use crate::model::ScheduledPromptRow;
use codex_protocol::ThreadId;

impl StateRuntime {
    pub async fn create_scheduled_prompt(
        &self,
        params: &ScheduledPromptCreateParams,
    ) -> anyhow::Result<ScheduledPrompt> {
        let now = Utc::now().timestamp();
        let interval_seconds = i64::try_from(params.interval_seconds)
            .map_err(|_| anyhow::anyhow!("invalid interval_seconds value"))?;
        sqlx::query(
            r#"
INSERT INTO scheduled_prompts (
    id,
    thread_id,
    rollout_path,
    prompt,
    interval_seconds,
    next_run_at,
    created_at,
    updated_at,
    last_run_started_at,
    last_run_completed_at,
    last_error,
    run_count,
    cancelled_at,
    lease_owner,
    lease_until
) VALUES (?, ?, ?, ?, ?, ?, ?, ?, NULL, NULL, NULL, 0, NULL, NULL, NULL)
            "#,
        )
        .bind(params.id.as_str())
        .bind(params.thread_id.to_string())
        .bind(params.rollout_path.to_string_lossy().to_string())
        .bind(params.prompt.as_str())
        .bind(interval_seconds)
        .bind(params.next_run_at.timestamp())
        .bind(now)
        .bind(now)
        .execute(self.pool.as_ref())
        .await?;

        let id = params.id.as_str();
        self.get_scheduled_prompt(id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("failed to load created scheduled prompt {id}"))
    }

    pub async fn get_scheduled_prompt(&self, id: &str) -> anyhow::Result<Option<ScheduledPrompt>> {
        let row = sqlx::query_as::<_, ScheduledPromptRow>(
            r#"
SELECT
    id,
    thread_id,
    rollout_path,
    prompt,
    interval_seconds,
    next_run_at,
    created_at,
    updated_at,
    last_run_started_at,
    last_run_completed_at,
    last_error,
    run_count,
    cancelled_at,
    lease_owner,
    lease_until
FROM scheduled_prompts
WHERE id = ?
            "#,
        )
        .bind(id)
        .fetch_optional(self.pool.as_ref())
        .await?;
        row.map(ScheduledPrompt::try_from).transpose()
    }

    pub async fn list_scheduled_prompts(
        &self,
        thread_id: Option<&ThreadId>,
    ) -> anyhow::Result<Vec<ScheduledPrompt>> {
        let rows: Vec<ScheduledPromptRow> = match thread_id {
            Some(thread_id) => {
                sqlx::query_as::<_, ScheduledPromptRow>(
                    r#"
SELECT
    id,
    thread_id,
    rollout_path,
    prompt,
    interval_seconds,
    next_run_at,
    created_at,
    updated_at,
    last_run_started_at,
    last_run_completed_at,
    last_error,
    run_count,
    cancelled_at,
    lease_owner,
    lease_until
FROM scheduled_prompts
WHERE thread_id = ?
ORDER BY created_at DESC, id DESC
                    "#,
                )
                .bind(thread_id.to_string())
                .fetch_all(self.pool.as_ref())
                .await?
            }
            None => {
                sqlx::query_as::<_, ScheduledPromptRow>(
                    r#"
SELECT
    id,
    thread_id,
    rollout_path,
    prompt,
    interval_seconds,
    next_run_at,
    created_at,
    updated_at,
    last_run_started_at,
    last_run_completed_at,
    last_error,
    run_count,
    cancelled_at,
    lease_owner,
    lease_until
FROM scheduled_prompts
ORDER BY created_at DESC, id DESC
                    "#,
                )
                .fetch_all(self.pool.as_ref())
                .await?
            }
        };
        rows.into_iter().map(ScheduledPrompt::try_from).collect()
    }

    pub async fn cancel_scheduled_prompt(&self, id: &str) -> anyhow::Result<bool> {
        let now = Utc::now().timestamp();
        let result = sqlx::query(
            r#"
UPDATE scheduled_prompts
SET cancelled_at = COALESCE(cancelled_at, ?), updated_at = ?
WHERE id = ? AND cancelled_at IS NULL
            "#,
        )
        .bind(now)
        .bind(now)
        .bind(id)
        .execute(self.pool.as_ref())
        .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn update_scheduled_prompt(
        &self,
        id: &str,
        prompt: Option<&str>,
        interval_seconds: Option<u64>,
    ) -> anyhow::Result<bool> {
        let now = Utc::now().timestamp();
        let interval_seconds = interval_seconds
            .map(i64::try_from)
            .transpose()
            .map_err(|_| anyhow::anyhow!("invalid interval_seconds value"))?;
        let result = sqlx::query(
            r#"
UPDATE scheduled_prompts
SET
    prompt = COALESCE(?, prompt),
    interval_seconds = COALESCE(?, interval_seconds),
    updated_at = ?
WHERE id = ? AND cancelled_at IS NULL
            "#,
        )
        .bind(prompt)
        .bind(interval_seconds)
        .bind(now)
        .bind(id)
        .execute(self.pool.as_ref())
        .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn claim_due_scheduled_prompts(
        &self,
        owner: &str,
        lease_seconds: i64,
        limit: usize,
    ) -> anyhow::Result<Vec<ScheduledPrompt>> {
        let now = Utc::now().timestamp();
        let candidate_rows: Vec<ScheduledPromptRow> = sqlx::query_as::<_, ScheduledPromptRow>(
            r#"
SELECT
    id,
    thread_id,
    rollout_path,
    prompt,
    interval_seconds,
    next_run_at,
    created_at,
    updated_at,
    last_run_started_at,
    last_run_completed_at,
    last_error,
    run_count,
    cancelled_at,
    lease_owner,
    lease_until
FROM scheduled_prompts
WHERE cancelled_at IS NULL
  AND next_run_at <= ?
ORDER BY next_run_at ASC, id ASC
LIMIT ?
            "#,
        )
        .bind(now)
        .bind(limit as i64)
        .fetch_all(self.pool.as_ref())
        .await?;

        let mut claimed = Vec::new();
        for row in candidate_rows {
            if row.is_leased(now) {
                continue;
            }
            let lease_until = now + lease_seconds;
            let result = sqlx::query(
                r#"
UPDATE scheduled_prompts
SET
    lease_owner = ?,
    lease_until = ?,
    last_run_started_at = ?,
    updated_at = ?,
    last_error = NULL
WHERE id = ?
  AND cancelled_at IS NULL
  AND next_run_at <= ?
  AND (lease_until IS NULL OR lease_until < ?)
                "#,
            )
            .bind(owner)
            .bind(lease_until)
            .bind(now)
            .bind(now)
            .bind(row.id.as_str())
            .bind(now)
            .bind(now)
            .execute(self.pool.as_ref())
            .await?;
            if result.rows_affected() == 0 {
                continue;
            }
            if let Some(job) = self.get_scheduled_prompt(row.id.as_str()).await? {
                claimed.push(job);
            }
        }
        Ok(claimed)
    }

    pub async fn finish_scheduled_prompt_run(
        &self,
        id: &str,
        owner: &str,
        next_run_at: chrono::DateTime<Utc>,
        last_error: Option<&str>,
    ) -> anyhow::Result<bool> {
        let now = Utc::now().timestamp();
        let result = sqlx::query(
            r#"
UPDATE scheduled_prompts
SET
    next_run_at = CASE WHEN cancelled_at IS NULL THEN ? ELSE next_run_at END,
    updated_at = ?,
    last_run_completed_at = ?,
    last_error = ?,
    run_count = run_count + 1,
    lease_owner = NULL,
    lease_until = NULL
WHERE id = ? AND lease_owner = ?
            "#,
        )
        .bind(next_run_at.timestamp())
        .bind(now)
        .bind(now)
        .bind(last_error)
        .bind(id)
        .bind(owner)
        .execute(self.pool.as_ref())
        .await?;
        Ok(result.rows_affected() > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ScheduledPromptStatus;
    use crate::runtime::test_support::unique_temp_dir;
    use chrono::Duration;
    use pretty_assertions::assert_eq;

    #[tokio::test]
    async fn create_list_claim_finish_and_cancel_scheduled_prompts() {
        let codex_home = unique_temp_dir();
        let runtime = StateRuntime::init(codex_home, "test-provider".to_string(), None)
            .await
            .expect("state runtime");
        let thread_id = ThreadId::new();
        let create = ScheduledPromptCreateParams {
            id: "job-1".to_string(),
            thread_id,
            rollout_path: PathBuf::from("/tmp/rollout-1.jsonl"),
            prompt: "check build".to_string(),
            interval_seconds: 600,
            next_run_at: Utc::now() - Duration::seconds(1),
        };
        let created = runtime
            .create_scheduled_prompt(&create)
            .await
            .expect("create scheduled prompt");
        assert_eq!(created.status, ScheduledPromptStatus::Active);
        assert_eq!(created.prompt, "check build");

        let listed = runtime
            .list_scheduled_prompts(Some(&thread_id))
            .await
            .expect("list scheduled prompts");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, created.id);

        let claimed = runtime
            .claim_due_scheduled_prompts("worker-a", 30, 10)
            .await
            .expect("claim scheduled prompt");
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].id, created.id);

        let finish_ok = runtime
            .finish_scheduled_prompt_run(
                created.id.as_str(),
                "worker-a",
                Utc::now() + Duration::seconds(600),
                None,
            )
            .await
            .expect("finish run");
        assert!(finish_ok);

        let after_finish = runtime
            .get_scheduled_prompt(created.id.as_str())
            .await
            .expect("get scheduled prompt")
            .expect("scheduled prompt exists");
        assert_eq!(after_finish.run_count, 1);
        assert_eq!(after_finish.status, ScheduledPromptStatus::Active);
        assert!(after_finish.last_run_completed_at.is_some());

        let cancel_ok = runtime
            .cancel_scheduled_prompt(created.id.as_str())
            .await
            .expect("cancel scheduled prompt");
        assert!(cancel_ok);

        let cancelled = runtime
            .get_scheduled_prompt(created.id.as_str())
            .await
            .expect("get cancelled scheduled prompt")
            .expect("cancelled prompt exists");
        assert_eq!(cancelled.status, ScheduledPromptStatus::Cancelled);
    }
}
