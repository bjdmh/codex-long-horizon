# Long-Horizon Experiments

This branch includes both deterministic integration tests and a reproducible
real-model benchmark for Execute mode.

## Deterministic coverage

The core Execute-mode integration tests live in:

- `codex-rs/core/tests/suite/execute_mode.rs`

They cover:

- auto-continuation when the model emits only a status update
- stopping on explicit `<task_complete>`
- stopping on explicit `<await_user_input>`
- recovering from a failed tool call without user input
- completing a multi-tool workflow without user input
- warning and exiting when the continuation limit is hit
- ignoring plain-text optional questions and continuing anyway
- ignoring unmarked “I think I’m done” text and continuing until explicitly done

## Real-model benchmark

Use:

- `scripts/run-long-horizon-experiments.sh`

The script runs two real Execute-mode tasks with the configured `CODEX_HOME`
experiment profile:

1. a Python bug-fix task that requires reproducing failing tests, editing code,
   and re-running tests until green
2. a file-edit task that requires making a change, verifying the result, and
   stopping without prompting for optional confirmation

The script fails if the agent:

- leaves the tests failing
- leaves the target file incorrect
- asks optional confirmation-style questions in the final transcript instead of
  continuing autonomously

Results are written under `/tmp/long-horizon-bench/` by default.
