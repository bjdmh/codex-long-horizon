# Non-interactive mode

For information about non-interactive mode, see [this documentation](https://developers.openai.com/codex/noninteractive).

Recurring scheduled prompts are also available in exec mode:

- `codex exec schedule create --last --every 10m "check CI and fix safe issues"`
- `codex exec schedule list`
- `codex exec schedule cancel <schedule_id>`
- `codex exec schedule serve`
