# Non-stop Runtime Handoff

Last updated: 2026-03-16

## Goal

This handoff documents the current state of the `Non-stop` runtime debugging
work. The user requirement is:

- `Non-stop` should keep working by default
- it should stop only when the model has explicitly searched for the next step
  and concluded there is genuinely no meaningful work left
- it must not hang forever after the model has already decided to stop

## What has already been changed

### Policy / prompt changes

- Added `docs/non-stop-execution-policy.md`
- Updated `docs/non-stop-mode.md`
- Updated `codex-rs/core/templates/collaboration_mode/non_stop.md`
- Updated `codex-rs/core/src/stream_events_utils.rs`

These changes shift `Non-stop` from “subtask complete means continue” toward:

- ordinary progress keeps going
- `<await_user_input>` means blocked
- `<task_complete>` means “I searched for the next step and found nothing
  meaningful left”

### Runtime / protocol changes already in progress

- Added `TurnCompleteReason` in `codex-rs/protocol/src/protocol.rs`
- Began wiring structured completion reasons into core and exec
- Added `Non-stop` checkpoint caching in
  `codex-rs/core/src/non_stop_checkpoint.rs`

### Dirty working tree touched so far

At the time of writing, these files are modified:

- `codex-rs/protocol/src/protocol.rs`
- `codex-rs/core/src/lib.rs`
- `codex-rs/core/src/codex.rs`
- `codex-rs/core/src/non_stop_checkpoint.rs`
- `codex-rs/core/src/state/turn.rs`
- `codex-rs/core/src/stream_events_utils.rs`
- `codex-rs/core/src/tasks/mod.rs`
- `codex-rs/core/src/tasks/regular.rs`
- `codex-rs/core/templates/collaboration_mode/non_stop.md`
- `codex-rs/core/tests/suite/execute_mode.rs`
- `codex-rs/core/tests/suite/resume_warning.rs`
- `codex-rs/core/src/agent/control.rs`
- `codex-rs/core/src/codex/rollout_reconstruction_tests.rs`
- `codex-rs/exec/src/lib.rs`
- `codex-rs/exec/tests/event_processor_with_json_output.rs`
- `codex-rs/exec/tests/suite/ephemeral.rs`
- `codex-rs/app-server-protocol/src/protocol/thread_history.rs`
- `codex-rs/tui/src/app.rs`
- `codex-rs/tui/src/chatwidget/tests.rs`
- `docs/non-stop-mode.md`
- `docs/non-stop-execution-policy.md`

## Confirmed findings

These findings are the most important part of the handoff.

### 1. The model really does emit `<task_complete>`

In real `paolu` runs, the raw assistant `response_item` contains
`<task_complete>...</task_complete>`.

This means the remaining bug is **not** “the model forgot to stop”.

### 2. `response.completed` can also arrive

In later instrumentation runs, the terminal response was observed together with
`response.completed`, with `assistant_control_signal=TaskComplete`.

So the remaining bug is **not always** “provider never sent completed”.

### 3. `run_turn` can reach and return

Instrumentation confirmed:

- `run_turn: breaking after completed sampling cycle`
- `run_turn: returning last_agent_message`

So the bug is not in the basic assistant-control parsing path.

### 4. `RegularTask::run` also receives the return value

Instrumentation confirmed:

- `regular task: run_turn returned to RegularTask::run`

This means the failure is later than `run_turn`.

### 5. A real checkpoint bottleneck existed

An earlier real root cause was found in
`codex-rs/core/src/non_stop_checkpoint.rs`:

- every relevant event re-read the checkpoint file from disk
- high-frequency event streams caused repeated reads on the same JSON file
- this could stall terminal event handling badly

The checkpoint path has already been partially improved with in-process caching.

### 6. Current remaining failure is in the turn-completion delivery chain

Even after:

- terminal signal detection
- terminal drain logic
- structured completion reasons
- checkpoint caching

the real process can still time out after the model has already concluded
“nothing meaningful remains”.

The remaining problem is somewhere between:

- `RegularTask::run` after `run_turn` returns
- `Session::on_task_finished(...)`
- `TurnComplete` event delivery
- `codex-exec` receiving and acting on that event

## Real-world reproduction commands

These are the minimal real runs repeatedly used for diagnosis.

### Direct `codex-exec` reproduction

Run in a temp repo containing:

- `calc.py` with buggy `add`
- `test_calc.py` with a single failing test
- `README.md`

Then invoke:

```bash
timeout 120s /root/code/spec-code/codex/codex-rs/target/debug/codex-exec \
  --non-stop \
  -C "$repo" \
  --skip-git-repo-check \
  --output-last-message "$repo/last.txt" \
  'Fix the bug, run tests, add one small sensible improvement, update the README, then explicitly search for the next useful step. Only if you find genuinely nothing meaningful left to do should you stop.'
```

Observed behavior:

- repo gets fixed correctly
- tests pass
- model emits a final “nothing meaningful left” summary
- process still exits by timeout (`124`) instead of cleanly

### Top-level `codex exec` reproduction

Same temp repo, using:

```bash
timeout 120s /root/code/spec-code/codex/codex-rs/target/debug/codex \
  -a never \
  -s workspace-write \
  exec --non-stop \
  -C "$repo" \
  --output-last-message "$repo/last.txt" \
  '...same prompt...'
```

This also timed out in real runs.

## Important environment facts

- Real `CODEX_HOME` used in repros: `/root/.paolu-codex`
- Active provider in real runs: `paolu`
- Relevant config file: `/root/.paolu-codex/config.toml`
- Provider stream idle timeout in config:
  `stream_idle_timeout_ms = 120000`

That long timeout makes the hang very obvious and expensive.

## Most likely remaining suspects

These are now the leading suspects, ordered by likelihood.

### A. `on_task_finished()` is still blocking internally

This is still the most likely place to inspect next. The exact question is:

- does `RegularTask::run` block inside `sess.on_task_finished(...)`
- and if yes, on what awaited operation?

The best next step is to temporarily add *minimal* step-by-step logs inside
`on_task_finished()` and remove them once the culprit is identified.

### B. `TurnComplete` is being generated but not consumed by exec

If `on_task_finished()` completes, the next suspect is:

- `Session::send_event(...)`
- `send_event_raw(...)`
- `codex-exec` event loop handling for `TurnComplete`

But this should only be investigated *after* confirming whether
`on_task_finished()` returns.

### C. Event ordering vs. prioritized terminal delivery

One attempted mitigation was to prioritize terminal events so `TurnComplete`
does not wait behind slow rollout persistence. That idea is still reasonable,
but it should only remain if it proves necessary after the root cause is fully
isolated.

## What not to assume

- Do **not** assume the bug is only prompt-related. It is not.
- Do **not** assume the provider always omits `response.completed`. Real runs
  showed both cases.
- Do **not** assume the top-level `codex` wrapper is the only issue. Direct
  `codex-exec` repros also time out.
- Do **not** assume checkpoint persistence is the only remaining issue. One
  real checkpoint bottleneck was found, but it did not fully solve the hang.

## Recommended next steps

1. Add precise logs inside `Session::on_task_finished(...)`:
   - entered
   - after active-turn removal
   - before replaying pending input
   - before sending `TurnComplete`
   - after sending `TurnComplete`

2. Re-run the minimal direct `codex-exec --non-stop` reproduction.

3. If `on_task_finished()` never returns:
   - identify the specific awaited call
   - move or relax that awaited work off the critical turn-completion path

4. If `on_task_finished()` does return:
   - instrument `codex-exec` around the `TurnComplete` receive path
   - confirm whether `CodexStatus::InitiateShutdown` and `ShutdownComplete`
     are actually observed

5. Only after the runtime chain is fixed, re-run the realistic greenfield /
   bugfix repos under `/tmp/nonstop-*` style temp dirs.

## Success criteria

The bug is actually fixed only when all of the following are true in a real run:

- model emits `<task_complete>`
- `TurnComplete` is emitted
- `completion_reason = NoMoreWork`
- `codex-exec` exits with `0` without external timeout
- `--output-last-message` is written
- the temp repo validates externally (e.g. tests pass)
