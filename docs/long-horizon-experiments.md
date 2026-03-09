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

On a fresh Debian/Ubuntu machine you can bootstrap the benchmark prerequisites
with:

- `just long-horizon-prereqs`

That installs the compiler/tooling needed by this repo plus Node 22, pytest,
and `just`.

To clone an existing CODEX_HOME into an Execute-mode experiment home, run:

- `just long-horizon-home`

You can override `SRC_HOME` and `DST_HOME` when needed.

For a single command that prepares prerequisites, creates the experiment home,
runs the full suite, and then checks health, use:

- `just long-horizon-all`

This is the easiest way to bring up the full long-horizon workflow on a fresh
machine.

Use:

- `scripts/run-long-horizon-experiments.sh`
- `just long-horizon-bench`

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
- `just long-horizon-history`

For the top-level suite history, run:

- `python3 scripts/summarize-long-horizon-suite-history.py`
- `just long-horizon-suite-history`

For a short current-health snapshot, run:

- `python3 scripts/long-horizon-status.py`
- `just long-horizon-status`

For a strict pass/fail health gate, run:

- `python3 scripts/check-long-horizon-health.py`
- `just long-horizon-health`

This command exits non-zero if benchmark regression warnings, soak warnings, or
optional confirmation regressions are present.

For a compact human-readable overview of the current benchmark, soak, and suite
artifacts, run:

- `python3 scripts/long-horizon-dashboard.py`
- `just long-horizon-dashboard`

You can also point it at a custom history file path.

## Soak runs

For repeated end-to-end runs against the real benchmark suite, use:

- `scripts/run-long-horizon-soak.sh`
- `just long-horizon-soak`

For a one-shot full run that executes the benchmark suite, a soak cycle, and
then writes a single combined report, use:

- `scripts/run-long-horizon-suite.sh`
- `just long-horizon-suite`

The suite runner also executes the strict health gate at the end and embeds the
health summary in the final `report.md` output. It also embeds the latest
benchmark summary and history trend report so the final suite artifact stands on
its own.

In addition to the Markdown report, the suite runner writes:

- `suite-summary.json`
- `suite-history.jsonl`

so the top-level end-to-end state can also be tracked over time.

## GitHub Actions

This branch also includes a manually-triggered workflow:

- `.github/workflows/long-horizon-suite.yml`

Use it when you want to run the full long-horizon suite in CI and collect the
final suite artifacts (`report.md`, summaries, history, and health output)
without logging into a machine and running the scripts by hand.

The workflow expects a repository secret named
`LONG_HORIZON_OPENAI_API_KEY`. Without it, the workflow fails fast before the
suite starts.

When the workflow finishes, it also publishes the generated `report.md` into
the GitHub Actions job summary so you can inspect the latest health state
without downloading the artifact first.

By default it runs the benchmark suite 3 times and writes:

- `/tmp/long-horizon-soak/soak-history.jsonl`
- `/tmp/long-horizon-soak/soak-summary.md`

Override `RUNS` if you want a shorter or longer soak cycle.

The soak runner also computes per-scenario stability metrics (currently duration
and exec-step standard deviation). If variability exceeds the configured
thresholds, it writes `soak-warnings.txt` and exits non-zero after finishing
the full soak cycle.

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
