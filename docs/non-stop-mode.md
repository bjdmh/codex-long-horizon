# Non-stop Mode

`Non-stop` is a new collaboration mode built on top of `Execute`.

`Execute` is good at finishing one concrete task inside one turn. `Non-stop` is
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

### Mode contract

- `Execute`: end the turn when the task is complete or blocked.
- `Non-stop`: treat task completion as a subtask boundary, not a stop signal.
- `Non-stop` only stops automatically when required human input is missing.
- Human `interrupt` / `shutdown` still stop it immediately.

### Phase 1: in-turn autonomous chaining

Phase 1 keeps the existing turn runtime and extends the collaboration contract:

- add `non_stop` as a first-class collaboration mode
- add dedicated built-in developer instructions
- reuse `<task_complete>` as "subtask complete; choose the next task"
- keep `<await_user_input>` as the only model-driven stop condition
- keep hard guardrails (continuation/stall limits) to avoid runaway loops

This phase is intentionally conservative: it proves the behavior change without
reintroducing the reverted loop scheduler.

### Phase 2: durable supervisor

After Phase 1 is stable, add a persistent supervisor above turn execution:

- durable goal / backlog / checkpoint state
- completion watcher that launches the next turn automatically
- restart recovery from rollout + state DB
- pause / sleep / retry scheduling that is policy-driven rather than prompt-only

### Phase 3: constrained self-directed innovation

Autonomous innovation should be explicit and bounded:

- generate candidate tasks only within the active goal scope
- record candidate tasks in backlog state before execution
- require budget / risk / relevance checks before self-started work
- surface innovation actions clearly in status / logs / artifacts

## Initial implementation plan

1. Add `Non-stop` mode to protocol, presets, docs, and UI mode lists.
2. Make the core turn loop distinguish `Execute` from `Non-stop`.
3. In `Non-stop`, continue automatically after `<task_complete>` with a
   developer nudge to select the next concrete task.
4. Keep `request_user_input` disabled in `Non-stop`.
5. Add deterministic tests for subtask chaining and blocker stops.
6. Only after this lands, start the durable supervisor work.

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
