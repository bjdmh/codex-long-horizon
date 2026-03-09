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
TRENDS_TXT="$WORK_BASE/trends.txt"
SUITE_JSON="$WORK_BASE/suite-summary.json"
SUITE_HISTORY_JSONL="$WORK_BASE/suite-history.jsonl"

rm -rf "$BENCH_DIR" "$SOAK_DIR"

CODEX_HOME_DIR="$CODEX_HOME_DIR" WORK_BASE="$BENCH_DIR" BENCHMARK_TIMEOUT_SECS=600 \
  "$ROOT_DIR/scripts/run-long-horizon-experiments.sh"

CODEX_HOME_DIR="$CODEX_HOME_DIR" WORK_BASE="$SOAK_DIR" RUNS="$SOAK_RUNS" \
  "$ROOT_DIR/scripts/run-long-horizon-soak.sh"

python3 "$ROOT_DIR/scripts/check-long-horizon-health.py" "$BENCH_DIR" "$SOAK_DIR" > "$HEALTH_TXT"
python3 "$ROOT_DIR/scripts/summarize-long-horizon-history.py" "$BENCH_DIR/history.jsonl" > "$TRENDS_TXT"

python3 - <<PY
import json
from pathlib import Path

bench_dir = Path(${BENCH_DIR@Q})
soak_dir = Path(${SOAK_DIR@Q})
report = Path(${REPORT_MD@Q})
health_txt = Path(${HEALTH_TXT@Q})
trends_txt = Path(${TRENDS_TXT@Q})

bench = json.loads((bench_dir / 'summary.json').read_text())
soak_summary = (soak_dir / 'soak-summary.md').read_text()
health_text = health_txt.read_text()

suite_summary = {
    'bench': bench,
    'health': health_text,
    'paths': {
        'bench_summary_md': str(bench_dir / 'summary.md'),
        'bench_summary_json': str(bench_dir / 'summary.json'),
        'bench_history_jsonl': str(bench_dir / 'history.jsonl'),
        'soak_summary_md': str(soak_dir / 'soak-summary.md'),
        'soak_history_jsonl': str(soak_dir / 'soak-history.jsonl'),
        'health_txt': str(health_txt),
        'trends_txt': str(trends_txt),
        'report_md': str(report),
    },
}
(Path(${SUITE_JSON@Q})).write_text(json.dumps(suite_summary, indent=2, ensure_ascii=False) + '\n', encoding='utf-8')
history_path = Path(${SUITE_HISTORY_JSONL@Q})
history_path.parent.mkdir(parents=True, exist_ok=True)
with history_path.open('a', encoding='utf-8') as handle:
    handle.write(json.dumps(suite_summary, ensure_ascii=False) + '\n')

lines = [
    '# Long-Horizon Suite Report',
    '',
    '## Bench Summary',
    '',
]

lines.extend((bench_dir / 'summary.md').read_text().splitlines())
lines.extend([
    '',
    '## Soak Summary',
    '',
])
lines.extend(soak_summary.splitlines())
lines.extend([
    '',
    '## Trends',
    '',
])
lines.extend(trends_txt.read_text().splitlines())
lines.extend([
    '',
    '## Health Gate',
    '',
])
lines.extend(health_text.splitlines())
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
printf ' - %s\n' "$TRENDS_TXT"
printf ' - %s\n' "$SUITE_JSON"
printf ' - %s\n' "$SUITE_HISTORY_JSONL"
printf ' - %s\n' "$REPORT_MD"
