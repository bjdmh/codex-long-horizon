# Non-interactive mode

For information about non-interactive mode, see [this documentation](https://developers.openai.com/codex/noninteractive).

Recurring scheduled loops are also available in exec mode:

- `codex exec ...` is the non-interactive subcommand exposed by the main `codex` binary.
- `codex exec schedule create --last --every 10m "check CI and fix safe issues"`
- `codex exec schedule list`
- `codex exec schedule cancel <schedule_id>`
- `codex exec schedule serve`

`codex exec schedule list` also shows one-shot scheduled tasks that Codex created internally via the `schedule` tool.

During non-interactive turns, the model can also use the `schedule` tool to create one-shot future tasks such as “check this log at 18:00 tomorrow”, and the `loop` tool to continue, pause, or complete autonomous follow-up work.
