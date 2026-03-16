#!/bin/bash
set -euo pipefail

export GIT_TERMINAL_PROMPT=0

if ! command -v gh >/dev/null 2>&1; then
  echo "error: github cli (gh) not installed" >&2
  exit 2
fi
if ! gh auth status --hostname github.com >/dev/null 2>&1; then
  echo "error: github cli (gh) not authenticated" >&2
  exit 3
fi

SRC_HOME=${SRC_HOME:-${CODEX_HOME:-$HOME/.codex}}
DST_HOME=${DST_HOME:-${SRC_HOME}-long-horizon}

if [ ! -f "$SRC_HOME/config.toml" ]; then
  echo "error: source CODEX_HOME config not found: $SRC_HOME/config.toml" >&2
  exit 4
fi

mkdir -p "$DST_HOME"
cp "$SRC_HOME/config.toml" "$DST_HOME/config.toml"
if [ -f "$SRC_HOME/AGENTS.md" ]; then
  cp "$SRC_HOME/AGENTS.md" "$DST_HOME/AGENTS.md"
fi
if [ -f "$SRC_HOME/auth.json" ]; then
  cp "$SRC_HOME/auth.json" "$DST_HOME/auth.json"
fi
if [ -f "$SRC_HOME/.credentials.json" ]; then
  cp "$SRC_HOME/.credentials.json" "$DST_HOME/.credentials.json"
fi

DST_HOME="$DST_HOME" python3 - <<'PY'
from pathlib import Path
import os

dst = Path(os.environ['DST_HOME'])
config = dst / 'config.toml'
text = config.read_text(encoding='utf-8')
setting = 'initial_collaboration_mode = "execute"'
model_setting = 'model = "gpt-5.4"'
lines = text.splitlines()

def ensure_root_setting(lines: list[str], key: str, setting: str) -> list[str]:
    root_section_end = next((i for i, line in enumerate(lines) if line.startswith('[')), len(lines))
    for i in range(root_section_end):
        if lines[i].startswith(f'{key} ='):
            lines[i] = setting
            return lines
    insert_at = 0
    for i, line in enumerate(lines[:root_section_end]):
        if line.startswith('service_tier ='):
            insert_at = i + 1
            break
    lines.insert(insert_at, setting)
    return lines

lines = ensure_root_setting(lines, 'initial_collaboration_mode', setting)
lines = ensure_root_setting(lines, 'model', model_setting)
config.write_text('\n'.join(lines) + '\n', encoding='utf-8')

readme = dst / 'README.md'
readme.write_text(
    'Long-horizon Codex experiment home.\n\n'
    'Usage:\n'
    f'- `CODEX_HOME={dst} codex`\n'
    f'- `CODEX_HOME={dst} codex exec "..."`\n\n'
    'This home mirrors the source configuration but defaults new sessions to execute mode with model gpt-5.4.\n',
    encoding='utf-8',
)
PY

printf 'Created long-horizon CODEX_HOME at %s\n' "$DST_HOME"
printf ' - %s\n' "$DST_HOME/config.toml"
printf ' - %s\n' "$DST_HOME/README.md"
