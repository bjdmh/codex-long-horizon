#!/bin/bash
set -euo pipefail

ROOT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
CODEX_HOME_DIR=${CODEX_HOME_DIR:-/root/.paolu-codex-long-horizon}
WORK_BASE=${WORK_BASE:-/tmp/long-horizon-bench}
CODEX_BIN=${CODEX_BIN:-$ROOT_DIR/codex-rs/target/debug/codex}

export GIT_TERMINAL_PROMPT=0

if ! command -v gh >/dev/null 2>&1; then
  echo "error: github cli (gh) not installed" >&2
  exit 2
fi
if ! gh auth status --hostname github.com >/dev/null 2>&1; then
  echo "error: github cli (gh) not authenticated" >&2
  exit 3
fi

mkdir -p "$WORK_BASE"

if [ ! -x "$CODEX_BIN" ]; then
  (cd "$ROOT_DIR/codex-rs" && cargo build -p codex-cli --bin codex)
fi

if ! command -v pytest >/dev/null 2>&1; then
  echo "error: pytest not installed" >&2
  exit 4
fi

if ! command -v node >/dev/null 2>&1; then
  echo "error: node not installed" >&2
  exit 5
fi

run_python_fix_benchmark() {
  local workdir="$WORK_BASE/python-fix"
  rm -rf "$workdir"
  mkdir -p "$workdir"
  cd "$workdir"

  cat > calculator.py <<'PY'
def add(a, b):
    return a - b


def safe_div(a, b):
    if b == 0:
        return 0
    return a / b
PY

  cat > test_calculator.py <<'PY'
from calculator import add, safe_div


def test_add():
    assert add(2, 3) == 5


def test_safe_div():
    assert safe_div(9, 3) == 3


def test_safe_div_zero():
    assert safe_div(9, 0) is None
PY

  git init >/dev/null
  git config user.name test
  git config user.email test@example.com
  git add calculator.py test_calculator.py
  git commit -m 'baseline failing fixture' >/dev/null

  python3 -m pytest -q > before.txt 2>&1 || true

  CODEX_HOME="$CODEX_HOME_DIR" "$CODEX_BIN" exec \
    "You are in execute mode. Fix the Python code so that all pytest tests pass. Do not ask me for confirmation. Decide the next steps yourself, run the necessary commands, and stop only when the task is actually complete." \
    > codex-output.txt 2>&1

  python3 -m pytest -q > after.txt 2>&1

  grep -q "3 passed" after.txt
  if grep -Eiq "(do you want me to continue|let me know if you want|waiting for your confirmation)" codex-output.txt; then
    echo "error: benchmark output shows optional confirmation-seeking behavior" >&2
    exit 10
  fi

  cat > result.json <<JSON
{
  "name": "python_fix",
  "workdir": "$workdir",
  "baseline": $(python3 - <<'PY'
import json
from pathlib import Path
print(json.dumps(Path('before.txt').read_text()))
PY
),
  "final": $(python3 - <<'PY'
import json
from pathlib import Path
print(json.dumps(Path('after.txt').read_text()))
PY
)
}
JSON
}

run_marked_completion_benchmark() {
  local workdir="$WORK_BASE/marked-completion"
  rm -rf "$workdir"
  mkdir -p "$workdir"
  cd "$workdir"

  cat > todo.txt <<'TXT'
replace me
TXT

  git init >/dev/null
  git config user.name test
  git config user.email test@example.com
  git add todo.txt
  git commit -m 'baseline todo' >/dev/null

  CODEX_HOME="$CODEX_HOME_DIR" "$CODEX_BIN" exec \
    "You are in execute mode. Replace the contents of todo.txt with the single line 'finished'. Verify the file contents after editing. Do not ask for confirmation and stop only when the task is complete." \
    > codex-output.txt 2>&1

  grep -qx 'finished' todo.txt
  if grep -Eiq "(do you want me to continue|let me know if you want|waiting for your confirmation)" codex-output.txt; then
    echo "error: benchmark output shows optional confirmation-seeking behavior" >&2
    exit 11
  fi

  cat > result.json <<JSON
{
  "name": "marked_completion",
  "workdir": "$workdir",
  "todo": $(python3 - <<'PY'
import json
from pathlib import Path
print(json.dumps(Path('todo.txt').read_text()))
PY
)
}
JSON
}

run_python_fix_benchmark
run_marked_completion_benchmark

printf 'Completed long-horizon experiments under %s\n' "$WORK_BASE"
printf ' - %s\n' "$WORK_BASE/python-fix/result.json"
printf ' - %s\n' "$WORK_BASE/marked-completion/result.json"
