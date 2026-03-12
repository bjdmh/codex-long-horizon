# Slash commands

For an overview of Codex CLI slash commands, see [this documentation](https://developers.openai.com/codex/cli/slash-commands).

This build also supports a recurring prompt command:

- `/loop 10m check deployment status`
- `/loop list`
- `/loop cancel <schedule_id>`

`/loop` is the user-facing control for recurring thread prompts. Separately, Codex can create autonomous scheduled tasks internally with the `schedule` tool and manage their wakeups with the `loop` tool.
