CREATE TABLE scheduled_prompts (
    id TEXT PRIMARY KEY,
    thread_id TEXT NOT NULL,
    rollout_path TEXT NOT NULL,
    prompt TEXT NOT NULL,
    interval_seconds INTEGER NOT NULL,
    next_run_at INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    last_run_started_at INTEGER,
    last_run_completed_at INTEGER,
    last_error TEXT,
    run_count INTEGER NOT NULL DEFAULT 0,
    cancelled_at INTEGER,
    lease_owner TEXT,
    lease_until INTEGER
);

CREATE INDEX idx_scheduled_prompts_thread_id
    ON scheduled_prompts(thread_id, created_at DESC, id DESC);

CREATE INDEX idx_scheduled_prompts_due
    ON scheduled_prompts(cancelled_at, next_run_at, lease_until);
