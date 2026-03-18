# Non-stop Execution Policy

This document defines the intended tradeoff for `Non-stop` mode:

- keep going by default
- do not stop at every local completion point
- stop only after the model actively searches for the next step and genuinely
  finds nothing left worth doing

## Policy

### Default behavior

`Non-stop` should assume there is probably another useful optimization step
after any single completed subtask.

The default outcome after a response finishes is therefore:

- continue if there is a concrete next action
- continue if there is uncertainty but a small validating action is available
- stop only for hard blockers or after an explicit "I searched for the next
  step and there is truly nothing left to do" conclusion

### Stop conditions

`Non-stop` should stop automatically in only three cases:

1. `await_user_input`
   - missing secrets, approvals, credentials, product decisions, or required
     external information
2. explicit empty-work completion
   - the agent actively searched for the next step and found none
   - the agent should then emit `<task_complete>...</task_complete>`
3. runtime guardrails
   - stall limit
   - continuation limit
   - interrupt / shutdown

### Meaning of `<task_complete>` in Non-stop

In `Long-Run`, `<task_complete>` means "the requested task is done."

In `Non-stop`, `<task_complete>` should be reserved for a stronger claim:

- the model searched for the next concrete step
- there is no meaningful next step left
- continuing would likely become repetition, empty-spin, or fake progress

It should **not** be used as a generic subtask boundary marker.

### Empty-spin definition

The run is in empty-spin if most candidate next actions are things like:

- repeating the same verification with no new hypothesis
- rephrasing documentation without changing capability or clarity materially
- minor refactors with no maintainability or correctness benefit
- speculative feature creep that is no longer meaningfully advancing the task
- status-only commentary without a new action

When the model reaches this state, it should stop with `<task_complete>`.

## Implementation notes

- Keep `Continue` as the normal response-complete control signal in `Non-stop`
- Treat `<task_complete>` as terminal in `Non-stop`
- Strengthen `Non-stop` prompts so the model explicitly searches for the next
  step before deciding there is none
- Preserve blocker-first stopping via `<await_user_input>`
- Keep stall detection as a safety net for cases where the model ignores the
  policy
