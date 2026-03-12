ALTER TABLE scheduled_prompts
    ADD COLUMN kind TEXT NOT NULL DEFAULT 'loop';

ALTER TABLE scheduled_prompts
    ADD COLUMN paused_until INTEGER;

ALTER TABLE scheduled_prompts
    ADD COLUMN completed_at INTEGER;

CREATE INDEX idx_scheduled_prompts_kind_status
    ON scheduled_prompts(kind, cancelled_at, completed_at, paused_until, next_run_at);
