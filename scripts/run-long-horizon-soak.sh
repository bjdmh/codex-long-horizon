#!/bin/bash
set -euo pipefail

ROOT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
RUNS=${RUNS:-3}
WORK_BASE=${WORK_BASE:-/tmp/long-horizon-soak}
BENCHMARK_SCRIPT=${BENCHMARK_SCRIPT:-$ROOT_DIR/scripts/run-long-horizon-experiments.sh}
CODEX_HOME_DIR=${CODEX_HOME_DIR:-${CODEX_HOME:-$HOME/.codex-long-horizon}}
MAX_DURATION_STDDEV=${MAX_DURATION_STDDEV:-30}
MAX_EXEC_STEPS_STDDEV=${MAX_EXEC_STEPS_STDDEV:-3}
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
summary_jsonl="$WORK_BASE/soak-history.jsonl"
summary_md="$WORK_BASE/soak-summary.md"
: > "$summary_jsonl"

run_one() {
  local index="$1"
  local run_dir="$WORK_BASE/run-$index"
  rm -rf "$run_dir"
  mkdir -p "$run_dir"
  if CODEX_HOME_DIR="$CODEX_HOME_DIR" WORK_BASE="$run_dir" "$BENCHMARK_SCRIPT"; then
    status="passed"
  else
    status="failed"
  fi

  python3 - <<PY >> "$summary_jsonl"
import json
from pathlib import Path

run_dir = Path(${run_dir@Q})
summary = json.loads((run_dir / 'summary.json').read_text())
summary['run_index'] = int(${index@Q})
summary['status'] = ${status@Q}
print(json.dumps(summary, ensure_ascii=False))
PY
}

for i in $(seq 1 "$RUNS"); do
  run_one "$i"
done

python3 - <<PY > "$summary_md"
import json
import statistics
from pathlib import Path

history = [json.loads(line) for line in Path(${summary_jsonl@Q}).read_text(encoding='utf-8').splitlines() if line.strip()]
rows = [
    '# Long-Horizon Soak Summary',
    '',
    '| Run | Status | Duration (s) | Exec Steps | Plan Updates | Optional Confirmations |',
    '| --- | --- | ---: | ---: | ---: | ---: |',
]
durations = []
exec_steps = []
plan_updates = []
confirmations = []
for item in history:
    totals = item['totals']
    durations.append(totals['duration_secs'])
    exec_steps.append(totals['exec_steps'])
    plan_updates.append(totals['plan_updates'])
    confirmations.append(totals['optional_confirmation_hits'])
    rows.append(
        f"| {item['run_index']} | {item['status']} | {totals['duration_secs']} | {totals['exec_steps']} | {totals['plan_updates']} | {totals['optional_confirmation_hits']} |"
    )

rows.extend([
    '',
    '## Aggregate',
    '',
    f"- runs: {len(history)}",
    f"- pass_rate: {sum(1 for item in history if item['status'] == 'passed')}/{len(history)}",
    f"- duration avg/max: {statistics.mean(durations):.2f}s / {max(durations)}s",
    f"- exec_steps avg/max: {statistics.mean(exec_steps):.2f} / {max(exec_steps)}",
    f"- plan_updates avg/max: {statistics.mean(plan_updates):.2f} / {max(plan_updates)}",
    f"- optional confirmations total: {sum(confirmations)}",
])

scenario_names = sorted({scenario['name'] for item in history for scenario in item.get('scenarios', [])})
rows.extend(['', '## Scenario Stability', ''])
regressions = []
for name in scenario_names:
    durations = []
    execs = []
    for item in history:
        for scenario in item.get('scenarios', []):
            if scenario['name'] == name:
                durations.append(scenario['duration_secs'])
                execs.append(scenario['metrics']['exec_steps'])
                break
    dur_stddev = statistics.pstdev(durations) if len(durations) > 1 else 0.0
    exec_stddev = statistics.pstdev(execs) if len(execs) > 1 else 0.0
    rows.append(f"- {name}: duration_stddev={dur_stddev:.2f}s, exec_stddev={exec_stddev:.2f}")
    if dur_stddev > float(${MAX_DURATION_STDDEV@Q}):
        regressions.append(f"{name}: duration stddev too high ({dur_stddev:.2f}s)")
    if exec_stddev > float(${MAX_EXEC_STEPS_STDDEV@Q}):
        regressions.append(f"{name}: exec step stddev too high ({exec_stddev:.2f})")

if regressions:
    rows.extend(['', '## Soak Warnings', ''])
    rows.extend(f"- {item}" for item in regressions)
    Path(${WORK_BASE@Q}).join('soak-warnings.txt').write_text('\n'.join(regressions) + '\n', encoding='utf-8')

Path(${summary_md@Q}).write_text('\n'.join(rows) + '\n', encoding='utf-8')
PY

if [ -f "$WORK_BASE/soak-warnings.txt" ]; then
  cat "$WORK_BASE/soak-warnings.txt" >&2
  OVERALL_EXIT_CODE=1
fi

printf 'Completed long-horizon soak runs under %s\n' "$WORK_BASE"
printf ' - %s\n' "$summary_jsonl"
printf ' - %s\n' "$summary_md"
if [ -f "$WORK_BASE/soak-warnings.txt" ]; then
  printf ' - %s\n' "$WORK_BASE/soak-warnings.txt"
fi
exit "$OVERALL_EXIT_CODE"
