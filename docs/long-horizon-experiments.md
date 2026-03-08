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

The script runs four real Execute-mode tasks with the configured `CODEX_HOME`
experiment profile:

1. a Python bug-fix task that requires reproducing failing tests, editing code,
   and re-running tests until green
2. a file-edit task that requires making a change, verifying the result, and
   stopping without prompting for optional confirmation
3. a multi-tool file task that requires editing and explicit shell verification
4. an already-done task that requires verifying success and stopping without
   making unnecessary changes
5. a dual-fix task that requires fixing multiple independent defects before
   stopping

The script fails if the agent:

- leaves the tests failing
- leaves the target file incorrect
- makes unnecessary edits in the already-done benchmark
- asks optional confirmation-style questions in the final transcript instead of
  continuing autonomously
- fails to show evidence of actually executing concrete actions (for example
  tool calls, patches, or file updates) in the transcript

Results are written under `/tmp/long-horizon-bench/` by default.

The benchmark also writes a compact Markdown summary to:

- `/tmp/long-horizon-bench/summary.md`

Use that file when you want a quick pass/fail overview, plus per-scenario
duration and execution-step counts, without opening each per-scenario JSON file.

For machine-readable trend tracking it also writes:

- `/tmp/long-horizon-bench/summary.json`
- `/tmp/long-horizon-bench/history.jsonl`

That aggregate file includes per-scenario metrics such as the number of exec
steps, patch applications, file updates, plan updates, and any optional
confirmation-style phrases detected in the transcript. It also records wall
clock duration for each scenario and the aggregate total.

Each benchmark run also appends the aggregate JSON onto `history.jsonl`, so you
can compare trends over time without manually collecting snapshots from prior
runs.

For a quick trend report over the accumulated history, run:

- `python3 scripts/summarize-long-horizon-history.py`

You can also point it at a custom history file path.

When a previous run exists, `summary.md` also includes a small “Delta Vs Previous
Run” section for the aggregate metrics so regressions are visible at a glance.

The script also applies lightweight regression thresholds to those deltas. If a
new run is materially slower or more verbose than the previous one, it records
`regression-warnings.txt` and exits non-zero after finishing the full benchmark
suite.

The benchmark script also enforces conservative guardrails on those metrics so
obvious regressions fail fast—for example, optional confirmation hits must stay
at zero, already-done tasks must not edit files, and simple scenarios must not
balloon into excessive execution loops.

If one scenario fails, the script keeps running the remaining scenarios and
records the failure in the Markdown summary before exiting non-zero at the end.
