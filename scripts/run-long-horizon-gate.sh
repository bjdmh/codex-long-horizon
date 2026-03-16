#!/bin/bash
set -euo pipefail

ROOT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
CODEX_HOME_DIR=${CODEX_HOME_DIR:-${CODEX_HOME:-$HOME/.codex-long-horizon}}
WORK_BASE=${WORK_BASE:-/tmp/long-horizon-gate}
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

CODEX_HOME_DIR="$CODEX_HOME_DIR" WORK_BASE="$WORK_BASE" SOAK_RUNS="$SOAK_RUNS" \
  bash "$ROOT_DIR/scripts/run-long-horizon-suite.sh"

python3 "$ROOT_DIR/scripts/check-long-horizon-health.py" "$WORK_BASE/bench" "$WORK_BASE/soak"

printf 'Long-horizon gate passed.\n'
printf ' - suite report: %s\n' "$WORK_BASE/report.md"
printf ' - dashboard: %s\n' "$WORK_BASE/dashboard.txt"
