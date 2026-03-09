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

SRC_HOME=${SRC_HOME:-$HOME/.paolu-codex}
DST_HOME=${DST_HOME:-$HOME/.paolu-codex-long-horizon}

if [ ! -f "$SRC_HOME/config.toml" ]; then
  echo "error: source CODEX_HOME config not found: $SRC_HOME/config.toml" >&2
  exit 4
fi

mkdir -p "$DST_HOME"
cp "$SRC_HOME/config.toml" "$DST_HOME/config.toml"
if [ -f "$SRC_HOME/AGENTS.md" ]; then
  cp "$SRC_HOME/AGENTS.md" "$DST_HOME/AGENTS.md"
fi

DST_HOME="$DST_HOME" python3 - <<'PY'
from pathlib import Path
import os

dst = Path(os.environ['DST_HOME'])
config = dst / 'config.toml'
text = config.read_text(encoding='utf-8')
setting = 'initial_collaboration_mode = "execute"'
if setting not in text:
    lines = text.splitlines()
    insert_at = 0
    for i, line in enumerate(lines):
        if line.startswith('service_tier ='):
            insert_at = i + 1
            break
    lines.insert(insert_at, setting)
    config.write_text('\n'.join(lines) + '\n', encoding='utf-8')

readme = dst / 'README.md'
readme.write_text(
    'Long-horizon Codex experiment home.\n\n'
    'Usage:\n'
    f'- `CODEX_HOME={dst} codex`\n'
    f'- `CODEX_HOME={dst} codex exec "..."`\n\n'
    'This home mirrors the source configuration but defaults new sessions to execute mode.\n',
    encoding='utf-8',
)
PY

printf 'Created long-horizon CODEX_HOME at %s\n' "$DST_HOME"
printf ' - %s\n' "$DST_HOME/config.toml"
printf ' - %s\n' "$DST_HOME/README.md"
