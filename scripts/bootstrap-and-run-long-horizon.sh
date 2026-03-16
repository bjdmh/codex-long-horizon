#!/bin/bash
set -euo pipefail

ROOT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
SRC_HOME=${SRC_HOME:-${CODEX_HOME:-$HOME/.codex}}
DST_HOME=${DST_HOME:-${SRC_HOME}-long-horizon}
SUITE_WORK_BASE=${SUITE_WORK_BASE:-/tmp/long-horizon-suite}
BENCH_RUNS=${BENCH_RUNS:-1}
SOAK_RUNS=${SOAK_RUNS:-1}

export GIT_TERMINAL_PROMPT=0

if ! command -v gh >/dev/null 2>&1; then
  echo "error: github cli (gh) not installed" >&2
  exit 2
fi
if ! gh auth status --hostname github.com >/dev/null 2>&1; then
  echo "error: github cli (gh) not authenticated" >&2
  exit 3
fi

SRC_HOME="$SRC_HOME" DST_HOME="$DST_HOME" bash "$ROOT_DIR/scripts/create-long-horizon-home.sh"
bash "$ROOT_DIR/scripts/install-long-horizon-prereqs.sh"
CODEX_HOME_DIR="$DST_HOME" BENCH_RUNS="$BENCH_RUNS" SOAK_RUNS="$SOAK_RUNS" WORK_BASE="$SUITE_WORK_BASE" \
  bash "$ROOT_DIR/scripts/run-long-horizon-suite.sh"
python3 "$ROOT_DIR/scripts/check-long-horizon-health.py" "$SUITE_WORK_BASE/bench" "$SUITE_WORK_BASE/soak"

printf 'Bootstrap-and-run completed.\n'
printf ' - CODEX_HOME: %s\n' "$DST_HOME"
printf ' - suite report: %s\n' "$SUITE_WORK_BASE/report.md"
