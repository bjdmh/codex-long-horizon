#!/usr/bin/env python3
import json
import sys
from pathlib import Path


def read_text(path: Path) -> str:
    return path.read_text(encoding="utf-8") if path.exists() else ""


def read_json(path: Path):
    return json.loads(path.read_text(encoding="utf-8")) if path.exists() else None


def main() -> int:
    bench_dir = Path(sys.argv[1]) if len(sys.argv) > 1 else Path("/tmp/long-horizon-bench")
    soak_dir = Path(sys.argv[2]) if len(sys.argv) > 2 else Path("/tmp/long-horizon-soak")
    suite_dir = Path(sys.argv[3]) if len(sys.argv) > 3 else Path("/tmp/long-horizon-suite")

    bench = read_json(bench_dir / "summary.json")
    suite = read_json(suite_dir / "suite-summary.json")
    health = read_text(suite_dir / "health.txt") or read_text(bench_dir / "regression-warnings.txt")

    print("Long-Horizon Dashboard")
    print("")

    if bench:
        totals = bench["totals"]
        print("[Bench]")
        print(f"- duration_secs: {totals['duration_secs']}")
        print(f"- exec_steps: {totals['exec_steps']}")
        print(f"- apply_patch_steps: {totals['apply_patch_steps']}")
        print(f"- plan_updates: {totals['plan_updates']}")
        print(f"- optional_confirmation_hits: {totals['optional_confirmation_hits']}")
    else:
        print("[Bench]")
        print("- missing summary.json")

    print("")
    print("[Health]")
    if health:
        for line in health.splitlines():
            print(f"- {line}")
    else:
        print("- no health output found")

    print("")
    print("[Suite]")
    if suite:
        totals = suite["bench"]["totals"]
        print(f"- bench duration_secs: {totals['duration_secs']}")
        print(f"- bench exec_steps: {totals['exec_steps']}")
        print(f"- bench plan_updates: {totals['plan_updates']}")
        print(f"- bench optional_confirmation_hits: {totals['optional_confirmation_hits']}")
    else:
        print("- missing suite-summary.json")

    print("")
    print("[Artifacts]")
    for path in [
        bench_dir / "summary.md",
        bench_dir / "summary.json",
        bench_dir / "history.jsonl",
        soak_dir / "soak-summary.md",
        soak_dir / "soak-history.jsonl",
        suite_dir / "report.md",
        suite_dir / "suite-summary.json",
        suite_dir / "suite-history.jsonl",
    ]:
        status = "present" if path.exists() else "missing"
        print(f"- {path}: {status}")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
