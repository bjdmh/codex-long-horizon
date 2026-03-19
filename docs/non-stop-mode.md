# Non-stop Mode

`Non-stop` is a new collaboration mode built on top of `Long-Run`.

`Long-Run` is good at finishing one concrete task inside one turn. `Non-stop` is
for long-lived autonomy: once a task is finished, the agent should immediately
pick the next concrete task that moves the user's goal forward instead of
stopping for optional confirmation.

## Problem

The current long-horizon branch improves single-turn execution, but it still
optimizes for "finish this task and stop". That is not enough for an agent that
should keep working for hours, continue across self-defined subtasks, and
occasionally generate the next useful task on its own.

The earlier loop/wakeup experiments were valuable, but they coupled too much of
the autonomy policy to one fragile continuation mechanism. `Non-stop` should be
introduced in layers.

## Design

See also:

- [Non-stop execution policy](./non-stop-execution-policy.md)

### Mode contract

- `Long-Run`: end the turn when the task is complete or blocked.
- `Non-stop`: keep searching for the next useful step instead of stopping at the
  first local completion point.
- `Non-stop` stops automatically only when required human input is missing, or
  when the user's requested outcome is actually achieved and verified.
- Human `interrupt` / `shutdown` still stop it immediately.

### Phase 1: in-turn autonomous chaining

Phase 1 keeps the existing turn runtime and extends the collaboration contract:

- add `non_stop` as a first-class collaboration mode
- add dedicated built-in developer instructions
- keep ordinary response completion as "search for the next task and continue"
- use `<task_complete>` to end the current turn while keeping the goal active
- use `<goal_complete>` only for true goal completion
- keep `<await_user_input>` as the blocker stop condition and `<goal_complete>`
  as the success stop condition
- keep hard guardrails (continuation/stall limits) to avoid runaway loops

This phase is intentionally conservative: it proves the behavior change without
reintroducing the reverted loop scheduler.

### Phase 2: durable supervisor

Phase 2 now adds a persistent supervisor above turn execution:

- durable goal + checkpoint state under `CODEX_HOME/non-stop-checkpoints`
- completion watcher in `codex exec --non-stop` that launches the next turn
  automatically after a completed turn
- blocker-aware stop logic: if the latest raw assistant output ended with
  `<await_user_input>`, the supervisor does not auto-resume
- restart recovery: `codex exec resume --non-stop --last` can synthesize the
  next supervisor prompt from the persisted goal prompt and the resumed thread
  state

Each per-thread `Non-stop` checkpoint lives at:

- `CODEX_HOME/non-stop-checkpoints/<thread_id>.json`

These checkpoints persist:

- the active goal prompt
- the latest turn status
- the latest visible assistant summary
- the latest raw assistant control signal (`continue`, `task_complete`,
  `goal_complete`, or `await_user_input`)

That gives the supervisor enough durable state to resume from an explicit
objective after process restarts, while still stopping automatically when human
input is actually required.

### Current stop policy

`Non-stop` should now bias toward continuing useful optimization work. In
practice:

- an ordinary response completion should keep the turn moving
- `<await_user_input>` remains the blocker stop signal
- `<task_complete>` in `Non-stop` should end the current turn while keeping the
  active goal alive
- `<goal_complete>` in `Non-stop` should be reserved for "the user's requested
  outcome is actually complete"

### Phase 3: constrained self-directed innovation

Autonomous innovation should be explicit and bounded:

- generate candidate tasks only within the active goal scope
- record candidate tasks in backlog state before execution
- require budget / risk / relevance checks before self-started work
- surface innovation actions clearly in status / logs / artifacts

Current implementation:

- `Non-stop` checkpoints now persist a time budget window and an innovation backlog
- the default budget is 48 hours unless the user explicitly gives a different duration
- a new user message resets the budget timer; resume-without-new-input keeps the existing timer
- bounded self-directed innovation is gated by the explicit CLI flag `--self-directed-innovation`
  and only works when `Non-stop` is enabled via `--non-stop` or config
- self-directed innovation is recorded via `<innovation_candidate>...</innovation_candidate>`
  before execution, then handed to the next `Non-stop` turn with `<task_complete>`
- supervisor prompts surface remaining budget, pending innovation items, and lightweight
  risk checks that block only obviously high-risk or over-budget innovation ideas

## Initial implementation plan

1. Add `Non-stop` mode to protocol, presets, docs, and UI mode lists.
2. Make the core turn loop distinguish `Long-Run` from `Non-stop`.
3. In `Non-stop`, continue automatically after ordinary response completion.
4. Reserve `<task_complete>` for "I searched for the next step and there is
   nothing meaningful left to do."
5. Keep `request_user_input` disabled in `Non-stop`.
6. Add deterministic tests for continuation and blocker stops.

## Guardrails

- `Non-stop` must keep a higher but still finite continuation cap in Phase 1.
- stall detection remains enabled
- optional plain-text "should I continue?" questions still do not pause the run
- the mode must remain visible in TUI / app-server APIs so clients can observe it

## CLI usage

Use:

- `codex --non-stop`
- `codex exec --non-stop "your goal"`

These entry points keep using the current `CODEX_HOME` and run with:

- `initial_collaboration_mode = "non_stop"` for the current invocation
- default model `gpt-5.4` unless you explicitly pass `--model`

If you want an isolated home for testing, create that derived `CODEX_HOME`
yourself first and then invoke `codex exec --non-stop` against it.
