#!/usr/bin/env python3
import json
import sys
from pathlib import Path


def read_json(path: Path):
    if not path.exists():
        raise FileNotFoundError(path)
    return json.loads(path.read_text(encoding="utf-8"))


def main() -> int:
    bench_dir = Path(sys.argv[1]) if len(sys.argv) > 1 else Path("/tmp/long-horizon-bench")
    soak_dir = Path(sys.argv[2]) if len(sys.argv) > 2 else Path("/tmp/long-horizon-soak")

    summary = read_json(bench_dir / "summary.json")
    totals = summary["totals"]
    failures: list[str] = []

    if totals.get("optional_confirmation_hits", 0) != 0:
        failures.append("optional confirmation hits must remain at zero")

    regression_warnings = bench_dir / "regression-warnings.txt"
    if regression_warnings.exists():
        failures.append(f"benchmark regression warnings present: {regression_warnings}")

    soak_warnings = soak_dir / "soak-warnings.txt"
    if soak_warnings.exists():
        failures.append(f"soak warnings present: {soak_warnings}")

    history_path = bench_dir / "history.jsonl"
    if history_path.exists():
        history_lines = [line for line in history_path.read_text(encoding="utf-8").splitlines() if line.strip()]
        if len(history_lines) < 1:
            failures.append("benchmark history is unexpectedly empty")
    else:
        failures.append(f"benchmark history file missing: {history_path}")

    print("Long-horizon health check")
    print(f"- benchmark summary: {bench_dir / 'summary.json'}")
    print(f"- soak dir: {soak_dir}")
    print(f"- optional_confirmation_hits: {totals.get('optional_confirmation_hits', 0)}")
    print(f"- duration_secs: {totals.get('duration_secs', 'unknown')}")
    print(f"- exec_steps: {totals.get('exec_steps', 'unknown')}")
    print(f"- plan_updates: {totals.get('plan_updates', 'unknown')}")

    if failures:
        print("- status: FAIL")
        for item in failures:
            print(f"  - {item}")
        return 1

    print("- status: PASS")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
