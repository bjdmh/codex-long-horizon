#!/bin/bash
set -euo pipefail

ROOT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
CODEX_HOME_DIR=${CODEX_HOME_DIR:-/root/.paolu-codex-long-horizon}
WORK_BASE=${WORK_BASE:-/tmp/long-horizon-bench}
CODEX_BIN=${CODEX_BIN:-$ROOT_DIR/codex-rs/target/debug/codex}
SUMMARY_PATH=${SUMMARY_PATH:-$WORK_BASE/summary.md}

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
printf '# Long-Horizon Benchmark Summary\n\n' > "$SUMMARY_PATH"
printf '| Scenario | Status | Notes |\n' >> "$SUMMARY_PATH"
printf '| --- | --- | --- |\n' >> "$SUMMARY_PATH"

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

assert_no_optional_confirmation_language() {
  local transcript_path="$1"
  if grep -Eiq "(do you want me to continue|let me know if you want|waiting for your confirmation)" "$transcript_path"; then
    echo "error: benchmark output shows optional confirmation-seeking behavior" >&2
    exit 10
  fi
}

assert_transcript_shows_execution() {
  local transcript_path="$1"
  if ! grep -Eiq "(^exec$|^apply_patch\(|^file update$|^Plan update$|^thinking$)" "$transcript_path"; then
    echo "error: benchmark transcript did not show evidence of autonomous execution steps" >&2
    exit 13
  fi
}

append_summary() {
  local name="$1"
  local status="$2"
  local note="$3"
  printf '| %s | %s | %s |\n' "$name" "$status" "$note" >> "$SUMMARY_PATH"
}

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
  assert_no_optional_confirmation_language codex-output.txt
  assert_transcript_shows_execution codex-output.txt

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
  append_summary "python_fix" "passed" "Fixed failing pytest suite autonomously"
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
  assert_no_optional_confirmation_language codex-output.txt
  assert_transcript_shows_execution codex-output.txt

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
  append_summary "marked_completion" "passed" "Edited target file and stopped after verification"
}

run_multi_tool_benchmark() {
  local workdir="$WORK_BASE/multi-tool"
  rm -rf "$workdir"
  mkdir -p "$workdir"
  cd "$workdir"

  cat > notes.txt <<'TXT'
before
TXT

  git init >/dev/null
  git config user.name test
  git config user.email test@example.com
  git add notes.txt
  git commit -m 'baseline notes' >/dev/null

  CODEX_HOME="$CODEX_HOME_DIR" "$CODEX_BIN" exec \
    "You are in execute mode. Update notes.txt so it contains only the line 'after'. Verify the final contents with a shell command. Do not ask for confirmation and stop only when the task is complete." \
    > codex-output.txt 2>&1

  grep -qx 'after' notes.txt
  assert_no_optional_confirmation_language codex-output.txt
  assert_transcript_shows_execution codex-output.txt

  cat > result.json <<JSON
{
  "name": "multi_tool",
  "workdir": "$workdir",
  "notes": $(python3 - <<'PY'
import json
from pathlib import Path
print(json.dumps(Path('notes.txt').read_text()))
PY
)
}
JSON
  append_summary "multi_tool" "passed" "Completed multi-tool workflow without confirmation"
}

run_already_done_benchmark() {
  local workdir="$WORK_BASE/already-done"
  rm -rf "$workdir"
  mkdir -p "$workdir"
  cd "$workdir"

  cat > calculator.py <<'PY'
def add(a, b):
    return a + b


def safe_div(a, b):
    if b == 0:
        return None
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
  git commit -m 'baseline already green' >/dev/null

  python3 -m pytest -q > before.txt 2>&1

  CODEX_HOME="$CODEX_HOME_DIR" "$CODEX_BIN" exec \
    "You are in execute mode. Verify that this repository already satisfies the tests. Do not make unnecessary changes. Stop once you have verified completion." \
    > codex-output.txt 2>&1

  python3 -m pytest -q > after.txt 2>&1
  grep -q '3 passed' after.txt
  if ! git diff --quiet; then
    echo "error: already-done benchmark produced unnecessary changes" >&2
    git diff >&2
    exit 12
  fi
  assert_no_optional_confirmation_language codex-output.txt
  assert_transcript_shows_execution codex-output.txt

  cat > result.json <<JSON
{
  "name": "already_done",
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
  append_summary "already_done" "passed" "Detected complete state without unnecessary edits"
}

run_python_fix_benchmark
run_marked_completion_benchmark
run_multi_tool_benchmark
run_already_done_benchmark

printf 'Completed long-horizon experiments under %s\n' "$WORK_BASE"
printf ' - %s\n' "$WORK_BASE/python-fix/result.json"
printf ' - %s\n' "$WORK_BASE/marked-completion/result.json"
printf ' - %s\n' "$WORK_BASE/multi-tool/result.json"
printf ' - %s\n' "$WORK_BASE/already-done/result.json"
printf ' - %s\n' "$SUMMARY_PATH"
