#!/bin/bash
set -euo pipefail

ROOT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
CODEX_HOME_DIR=${CODEX_HOME_DIR:-/root/.paolu-codex-long-horizon}
WORK_BASE=${WORK_BASE:-/tmp/long-horizon-suite}
BENCH_RUNS=${BENCH_RUNS:-1}
SOAK_RUNS=${SOAK_RUNS:-3}

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

BENCH_DIR="$WORK_BASE/bench"
SOAK_DIR="$WORK_BASE/soak"
REPORT_MD="$WORK_BASE/report.md"
HEALTH_TXT="$WORK_BASE/health.txt"

rm -rf "$BENCH_DIR" "$SOAK_DIR"

CODEX_HOME_DIR="$CODEX_HOME_DIR" WORK_BASE="$BENCH_DIR" BENCHMARK_TIMEOUT_SECS=600 \
  "$ROOT_DIR/scripts/run-long-horizon-experiments.sh"

CODEX_HOME_DIR="$CODEX_HOME_DIR" WORK_BASE="$SOAK_DIR" RUNS="$SOAK_RUNS" \
  "$ROOT_DIR/scripts/run-long-horizon-soak.sh"

python3 "$ROOT_DIR/scripts/check-long-horizon-health.py" "$BENCH_DIR" "$SOAK_DIR" > "$HEALTH_TXT"

python3 - <<PY
import json
from pathlib import Path

bench_dir = Path(${BENCH_DIR@Q})
soak_dir = Path(${SOAK_DIR@Q})
report = Path(${REPORT_MD@Q})

bench = json.loads((bench_dir / 'summary.json').read_text())

lines = [
    '# Long-Horizon Suite Report',
    '',
    '## Bench Totals',
    '',
    f"- duration_secs: {bench['totals']['duration_secs']}",
    f"- exec_steps: {bench['totals']['exec_steps']}",
    f"- apply_patch_steps: {bench['totals']['apply_patch_steps']}",
    f"- plan_updates: {bench['totals']['plan_updates']}",
    f"- optional_confirmation_hits: {bench['totals']['optional_confirmation_hits']}",
    '',
    '## Soak Summary',
    '',
]

lines.extend((soak_dir / 'soak-summary.md').read_text().splitlines())
lines.extend([
    '',
    '## Health Gate',
    '',
])
lines.extend(Path(${HEALTH_TXT@Q}).read_text().splitlines())
report.write_text('\n'.join(lines) + '\n', encoding='utf-8')
PY

printf 'Completed long-horizon suite under %s\n' "$WORK_BASE"
printf ' - %s\n' "$BENCH_DIR/summary.md"
printf ' - %s\n' "$BENCH_DIR/summary.json"
printf ' - %s\n' "$BENCH_DIR/history.jsonl"
printf ' - %s\n' "$SOAK_DIR/soak-summary.md"
printf ' - %s\n' "$SOAK_DIR/soak-history.jsonl"
if [ -f "$SOAK_DIR/soak-warnings.txt" ]; then
  printf ' - %s\n' "$SOAK_DIR/soak-warnings.txt"
fi
printf ' - %s\n' "$HEALTH_TXT"
printf ' - %s\n' "$REPORT_MD"
