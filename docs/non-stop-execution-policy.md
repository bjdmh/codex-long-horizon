# Non-stop Execution Policy

This document defines the intended tradeoff for `Non-stop` mode:

- keep going by default
- do not stop at every local completion point
- stop only after the user's goal is actually satisfied, or after the model
  reaches a real user-only blocker

## Policy

### Default behavior

`Non-stop` should assume there is probably another useful step after any single
completed subtask, and should treat the user's goal as still active until that
goal is actually achieved.

The default outcome after a response finishes is therefore:

- continue if there is a concrete next action
- continue if there is uncertainty but a small validating action is available
- continue if the current turn is done but the user's goal still needs further
  monitoring, verification, or retries
- stop only for hard blockers or after an explicit "the user's requested
  outcome is actually achieved" conclusion

### Stop conditions

`Non-stop` should stop automatically in only three cases:

1. `await_user_input`
   - missing secrets, approvals, credentials, product decisions, or required
     external information
2. explicit goal completion
   - the user-facing outcome has actually been achieved and verified
   - the agent should then emit `<goal_complete>...</goal_complete>`
3. runtime guardrails
   - stall limit
   - continuation limit
   - interrupt / shutdown

### Meaning of `<task_complete>` in Non-stop

In `Long-Run`, `<task_complete>` means "the requested task is done."

In `Non-stop`, `<task_complete>` means something narrower:

- the current turn finished a concrete step
- the overall user goal is still active
- the next step should be taken by the next Non-stop turn rather than by ending
  the run entirely

It should **not** be used as proof that the overall goal is complete.

### Meaning of `<goal_complete>` in Non-stop

`<goal_complete>` is the terminal success marker for `Non-stop`:

- the user's requested outcome is actually satisfied
- the agent has verified that success as well as the environment permits
- the run may now stop cleanly without dropping an unfinished watch/monitor goal

### Empty-spin definition

The run is in empty-spin if most candidate next actions are things like:

- repeating the same verification with no new hypothesis
- rephrasing documentation without changing capability or clarity materially
- minor refactors with no maintainability or correctness benefit
- speculative feature creep that is no longer meaningfully advancing the task
- status-only commentary without a new action

When the model reaches this state but the user's goal still is not complete, it
should hand off to the next Non-stop turn with `<task_complete>` rather than
claiming success.

## Implementation notes

- Keep `Continue` as the normal response-complete control signal in `Non-stop`
- Treat `<goal_complete>` as terminal success in `Non-stop`
- Treat `<task_complete>` as "end this turn, then keep going toward the same
  goal"
- Strengthen `Non-stop` prompts so the model explicitly checks whether the
  user's goal is actually complete before stopping
- Preserve blocker-first stopping via `<await_user_input>`
- Keep stall detection as a safety net for cases where the model ignores the
  policy
