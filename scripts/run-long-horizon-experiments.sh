#!/bin/bash
set -euo pipefail

ROOT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
CODEX_HOME_DIR=${CODEX_HOME_DIR:-/root/.paolu-codex-long-horizon}
WORK_BASE=${WORK_BASE:-/tmp/long-horizon-bench}
CODEX_BIN=${CODEX_BIN:-$ROOT_DIR/codex-rs/target/debug/codex}
SUMMARY_PATH=${SUMMARY_PATH:-$WORK_BASE/summary.md}
AGGREGATE_JSON=${AGGREGATE_JSON:-$WORK_BASE/summary.json}
BENCHMARK_TIMEOUT_SECS=${BENCHMARK_TIMEOUT_SECS:-600}
OVERALL_EXIT_CODE=0

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
printf '| Scenario | Status | Duration (s) | Exec Steps | Apply Patch | Plan Updates | Optional Confirmations | Notes |\n' >> "$SUMMARY_PATH"
printf '| --- | --- | ---: | ---: | ---: | ---: | ---: | --- |\n' >> "$SUMMARY_PATH"

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
  if ! grep -Eiq "(^exec$|^apply_patch\(|^file update$)" "$transcript_path"; then
    echo "error: benchmark transcript did not show evidence of concrete autonomous execution actions" >&2
    exit 13
  fi
}

append_summary() {
  local name="$1"
  local status="$2"
  local duration_secs="$3"
  local exec_steps="$4"
  local apply_patch_steps="$5"
  local plan_updates="$6"
  local confirmation_hits="$7"
  local note="$8"
  printf '| %s | %s | %s | %s | %s | %s | %s | %s |\n' \
    "$name" "$status" "$duration_secs" "$exec_steps" "$apply_patch_steps" "$plan_updates" "$confirmation_hits" "$note" >> "$SUMMARY_PATH"
}

append_summary_failure() {
  local name="$1"
  local note="$2"
  printf '| %s | failed | - | - | - | - | - | %s |\n' "$name" "$note" >> "$SUMMARY_PATH"
}

run_codex_exec() {
  local prompt="$1"
  local transcript_path="$2"
  local started_at finished_at duration exit_code
  started_at=$(date +%s)
  set +e
  timeout "${BENCHMARK_TIMEOUT_SECS}s" env CODEX_HOME="$CODEX_HOME_DIR" "$CODEX_BIN" exec "$prompt" > "$transcript_path" 2>&1
  exit_code=$?
  set -e
  finished_at=$(date +%s)
  duration=$((finished_at - started_at))
  if [ "$exit_code" -ne 0 ]; then
    echo "error: codex exec failed with exit code $exit_code after ${duration}s" >&2
    tail -n 200 "$transcript_path" >&2 || true
    exit "$exit_code"
  fi
  printf '%s' "$duration"
}

run_scenario() {
  local name="$1"
  shift
  if "$@"; then
    return 0
  fi

  OVERALL_EXIT_CODE=1
  local scenario_dir="$WORK_BASE/$name"
  local note="scenario failed"
  if [ -f "$scenario_dir/error.txt" ]; then
    note=$(tr '\n' ' ' < "$scenario_dir/error.txt" | sed 's/|/\//g' | cut -c1-160)
  fi
  append_summary_failure "$name" "$note"
  return 0
}

transcript_metric_count() {
  local transcript_path="$1"
  local pattern="$2"
  grep -Eic "$pattern" "$transcript_path" || true
}

write_result_with_metrics() {
  local result_path="$1"
  local name="$2"
  local extra_json="$3"
  local duration_secs="$4"
  EXTRA_JSON="$extra_json" DURATION_SECS="$duration_secs" python3 - <<PY > "$result_path"
import json
import os
from pathlib import Path

transcript = Path('codex-output.txt').read_text()
data = {
    'name': ${name@Q},
    'workdir': str(Path.cwd()),
    'duration_secs': int(os.environ['DURATION_SECS']),
    'metrics': {
        'exec_steps': sum(1 for line in transcript.splitlines() if line.strip() == 'exec'),
        'apply_patch_steps': transcript.count('apply_patch('),
        'file_updates': sum(1 for line in transcript.splitlines() if line.strip() == 'file update'),
        'plan_updates': sum(1 for line in transcript.splitlines() if line.strip() == 'Plan update'),
        'thinking_blocks': sum(1 for line in transcript.splitlines() if line.strip() == 'thinking'),
        'optional_confirmation_hits': sum(
            1
            for needle in [
                'do you want me to continue',
                'let me know if you want',
                'waiting for your confirmation',
            ]
            if needle in transcript.lower()
        ),
    },
}
extra = json.loads(os.environ['EXTRA_JSON'])
data.update(extra)
print(json.dumps(data, indent=2, ensure_ascii=False))
PY
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

  git init -b main >/dev/null
  git config user.name test
  git config user.email test@example.com
  git add calculator.py test_calculator.py
  git commit -m 'baseline failing fixture' >/dev/null

  python3 -m pytest -q > before.txt 2>&1 || true

  local duration_secs
  duration_secs=$(run_codex_exec \
    "You are in execute mode. Fix the Python code so that all pytest tests pass. Do not ask me for confirmation. Decide the next steps yourself, run the necessary commands, and stop only when the task is actually complete." \
    codex-output.txt)

  python3 -m pytest -q > after.txt 2>&1

  grep -q "3 passed" after.txt
  assert_no_optional_confirmation_language codex-output.txt
  assert_transcript_shows_execution codex-output.txt

  write_result_with_metrics \
    result.json \
    python_fix \
    "$(python3 -c 'import json, pathlib; print(json.dumps({"baseline": pathlib.Path("before.txt").read_text(), "final": pathlib.Path("after.txt").read_text()}))')" \
    "$duration_secs"
  append_summary "python_fix" "passed" "$duration_secs" 0 0 0 0 "Fixed failing pytest suite autonomously"
}

run_marked_completion_benchmark() {
  local workdir="$WORK_BASE/marked-completion"
  rm -rf "$workdir"
  mkdir -p "$workdir"
  cd "$workdir"

  cat > todo.txt <<'TXT'
replace me
TXT

  git init -b main >/dev/null
  git config user.name test
  git config user.email test@example.com
  git add todo.txt
  git commit -m 'baseline todo' >/dev/null

  local duration_secs
  duration_secs=$(run_codex_exec \
    "You are in execute mode. Replace the contents of todo.txt with the single line 'finished'. Verify the file contents after editing. Do not ask for confirmation and stop only when the task is complete." \
    codex-output.txt)

  grep -qx 'finished' todo.txt
  assert_no_optional_confirmation_language codex-output.txt
  assert_transcript_shows_execution codex-output.txt

  write_result_with_metrics \
    result.json \
    marked_completion \
    "$(python3 -c 'import json, pathlib; print(json.dumps({"todo": pathlib.Path("todo.txt").read_text()}))')" \
    "$duration_secs"
  append_summary "marked_completion" "passed" "$duration_secs" 0 0 0 0 "Edited target file and stopped after verification"
}

run_multi_tool_benchmark() {
  local workdir="$WORK_BASE/multi-tool"
  rm -rf "$workdir"
  mkdir -p "$workdir"
  cd "$workdir"

  cat > notes.txt <<'TXT'
before
TXT

  git init -b main >/dev/null
  git config user.name test
  git config user.email test@example.com
  git add notes.txt
  git commit -m 'baseline notes' >/dev/null

  local duration_secs
  duration_secs=$(run_codex_exec \
    "You are in execute mode. Update notes.txt so it contains only the line 'after'. Verify the final contents with a shell command. Do not ask for confirmation and stop only when the task is complete." \
    codex-output.txt)

  grep -qx 'after' notes.txt
  assert_no_optional_confirmation_language codex-output.txt
  assert_transcript_shows_execution codex-output.txt

  write_result_with_metrics \
    result.json \
    multi_tool \
    "$(python3 -c 'import json, pathlib; print(json.dumps({"notes": pathlib.Path("notes.txt").read_text()}))')" \
    "$duration_secs"
  append_summary "multi_tool" "passed" "$duration_secs" 0 0 0 0 "Completed multi-tool workflow without confirmation"
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

  git init -b main >/dev/null
  git config user.name test
  git config user.email test@example.com
  git add calculator.py test_calculator.py
  git commit -m 'baseline already green' >/dev/null

  python3 -m pytest -q > before.txt 2>&1

  local duration_secs
  duration_secs=$(run_codex_exec \
    "You are in execute mode. Verify that this repository already satisfies the tests. Do not make unnecessary changes. Stop once you have verified completion." \
    codex-output.txt)

  python3 -m pytest -q > after.txt 2>&1
  grep -q '3 passed' after.txt
  if ! git diff --quiet; then
    echo "error: already-done benchmark produced unnecessary changes" >&2
    git diff >&2
    exit 12
  fi
  assert_no_optional_confirmation_language codex-output.txt
  assert_transcript_shows_execution codex-output.txt

  write_result_with_metrics \
    result.json \
    already_done \
    "$(python3 -c 'import json, pathlib; print(json.dumps({"baseline": pathlib.Path("before.txt").read_text(), "final": pathlib.Path("after.txt").read_text()}))')" \
    "$duration_secs"
  append_summary "already_done" "passed" "$duration_secs" 0 0 0 0 "Detected complete state without unnecessary edits"
}

run_dual_fix_benchmark() {
  local workdir="$WORK_BASE/dual-fix"
  rm -rf "$workdir"
  mkdir -p "$workdir"
  cd "$workdir"

  cat > math_ops.py <<'PY'
def mul(a, b):
    return a + b


def sub(a, b):
    return a + b
PY

  cat > test_math_ops.py <<'PY'
from math_ops import mul, sub


def test_mul():
    assert mul(3, 4) == 12


def test_sub():
    assert sub(7, 2) == 5
PY

  git init -b main >/dev/null
  git config user.name test
  git config user.email test@example.com
  git add math_ops.py test_math_ops.py
  git commit -m 'baseline dual fix fixture' >/dev/null

  python3 -m pytest -q > before.txt 2>&1 || true

  local duration_secs
  duration_secs=$(run_codex_exec \
    "You are in execute mode. Fix all bugs so that the pytest suite passes. There are multiple independent defects. Do not ask for confirmation, keep going until everything is fixed, and stop only when the task is actually complete." \
    codex-output.txt)

  python3 -m pytest -q > after.txt 2>&1

  grep -q "2 passed" after.txt
  assert_no_optional_confirmation_language codex-output.txt
  assert_transcript_shows_execution codex-output.txt

  write_result_with_metrics \
    result.json \
    dual_fix \
    "$(python3 -c 'import json, pathlib; print(json.dumps({"baseline": pathlib.Path("before.txt").read_text(), "final": pathlib.Path("after.txt").read_text()}))')" \
    "$duration_secs"
  append_summary "dual_fix" "passed" "$duration_secs" 0 0 0 0 "Fixed multiple independent defects without waiting for input"
}

run_python_fix_benchmark_wrapper() {
  run_python_fix_benchmark 2> "$WORK_BASE/python-fix/error.txt"
}

run_marked_completion_benchmark_wrapper() {
  run_marked_completion_benchmark 2> "$WORK_BASE/marked-completion/error.txt"
}

run_multi_tool_benchmark_wrapper() {
  run_multi_tool_benchmark 2> "$WORK_BASE/multi-tool/error.txt"
}

run_already_done_benchmark_wrapper() {
  run_already_done_benchmark 2> "$WORK_BASE/already-done/error.txt"
}

run_dual_fix_benchmark_wrapper() {
  run_dual_fix_benchmark 2> "$WORK_BASE/dual-fix/error.txt"
}

run_scenario python_fix run_python_fix_benchmark_wrapper
run_scenario marked_completion run_marked_completion_benchmark_wrapper
run_scenario multi_tool run_multi_tool_benchmark_wrapper
run_scenario already_done run_already_done_benchmark_wrapper
run_scenario dual_fix run_dual_fix_benchmark_wrapper

python3 - <<PY > "$AGGREGATE_JSON"
import json
from pathlib import Path

base = Path(${WORK_BASE@Q})
results = []
for rel in ['python-fix', 'marked-completion', 'multi-tool', 'already-done', 'dual-fix']:
    result_path = base / rel / 'result.json'
    if result_path.exists():
        results.append(json.loads(result_path.read_text()))
aggregate = {
    'scenarios': results,
    'totals': {
        'exec_steps': sum(item['metrics']['exec_steps'] for item in results),
        'apply_patch_steps': sum(item['metrics']['apply_patch_steps'] for item in results),
        'file_updates': sum(item['metrics']['file_updates'] for item in results),
        'plan_updates': sum(item['metrics']['plan_updates'] for item in results),
        'thinking_blocks': sum(item['metrics']['thinking_blocks'] for item in results),
        'optional_confirmation_hits': sum(item['metrics']['optional_confirmation_hits'] for item in results),
        'duration_secs': sum(item['duration_secs'] for item in results),
    },
}
print(json.dumps(aggregate, indent=2, ensure_ascii=False))
PY

python3 - <<PY
import json
from pathlib import Path

summary_path = Path(${SUMMARY_PATH@Q})
aggregate = json.loads(Path(${AGGREGATE_JSON@Q}).read_text())
rows = [
    '| Scenario | Status | Duration (s) | Exec Steps | Apply Patch | Plan Updates | Optional Confirmations | Notes |',
    '| --- | --- | ---: | ---: | ---: | ---: | ---: | --- |',
]
notes = {
    'python_fix': 'Fixed failing pytest suite autonomously',
    'marked_completion': 'Edited target file and stopped after verification',
    'multi_tool': 'Completed multi-tool workflow without confirmation',
    'already_done': 'Detected complete state without unnecessary edits',
    'dual_fix': 'Fixed multiple independent defects without waiting for input',
}
for scenario in aggregate['scenarios']:
    metrics = scenario['metrics']
    rows.append(
        f"| {scenario['name']} | passed | {scenario['duration_secs']} | {metrics['exec_steps']} | {metrics['apply_patch_steps']} | {metrics['plan_updates']} | {metrics['optional_confirmation_hits']} | {notes.get(scenario['name'], '')} |"
    )
rows.append('')
rows.append('## Totals')
rows.append('')
totals = aggregate['totals']
rows.append(f"- Duration: {totals['duration_secs']}s")
rows.append(f"- Exec steps: {totals['exec_steps']}")
rows.append(f"- Apply patch steps: {totals['apply_patch_steps']}")
rows.append(f"- Plan updates: {totals['plan_updates']}")
rows.append(f"- Optional confirmations: {totals['optional_confirmation_hits']}")
summary_path.write_text('\n'.join(rows) + '\n')
PY

printf 'Completed long-horizon experiments under %s\n' "$WORK_BASE"
printf ' - %s\n' "$WORK_BASE/python-fix/result.json"
printf ' - %s\n' "$WORK_BASE/marked-completion/result.json"
printf ' - %s\n' "$WORK_BASE/multi-tool/result.json"
printf ' - %s\n' "$WORK_BASE/already-done/result.json"
printf ' - %s\n' "$WORK_BASE/dual-fix/result.json"
printf ' - %s\n' "$SUMMARY_PATH"
printf ' - %s\n' "$AGGREGATE_JSON"
exit "$OVERALL_EXIT_CODE"
