#!/usr/bin/env python3
import json
import sys
from pathlib import Path


def load_json(path: Path):
    if not path.exists():
        return None
    return json.loads(path.read_text(encoding="utf-8"))


def main() -> int:
    bench_dir = Path(sys.argv[1]) if len(sys.argv) > 1 else Path("/tmp/long-horizon-bench")
    soak_dir = Path(sys.argv[2]) if len(sys.argv) > 2 else Path("/tmp/long-horizon-soak")

    bench = load_json(bench_dir / "summary.json")
    if bench is None:
        print(f"benchmark summary not found: {bench_dir / 'summary.json'}", file=sys.stderr)
        return 1

    print("Long-horizon status")
    print(f"- bench generated_at: {bench.get('generated_at', 'unknown')}")
    totals = bench["totals"]
    print(f"- bench duration_secs: {totals['duration_secs']}")
    print(f"- bench exec_steps: {totals['exec_steps']}")
    print(f"- bench plan_updates: {totals['plan_updates']}")
    print(f"- bench optional_confirmation_hits: {totals['optional_confirmation_hits']}")

    regression_warnings = bench_dir / "regression-warnings.txt"
    if regression_warnings.exists():
        print(f"- bench warnings: {regression_warnings}")
    else:
        print("- bench warnings: none")

    soak_summary = soak_dir / "soak-summary.md"
    if soak_summary.exists():
        soak_text = soak_summary.read_text(encoding="utf-8")
        print("- soak summary: present")
        if "## Soak Warnings" in soak_text:
            print("- soak warnings: present")
        else:
            print("- soak warnings: none")
    else:
        print(f"- soak summary: missing ({soak_summary})")

    history = bench_dir / "history.jsonl"
    if history.exists():
        lines = [line for line in history.read_text(encoding="utf-8").splitlines() if line.strip()]
        print(f"- bench history runs: {len(lines)}")
    else:
        print("- bench history runs: 0")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
