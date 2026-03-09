#!/usr/bin/env python3
import json
import statistics
import sys
from pathlib import Path


def main() -> int:
    history_path = Path(sys.argv[1]) if len(sys.argv) > 1 else Path("/tmp/long-horizon-suite/suite-history.jsonl")
    if not history_path.exists():
        print(f"suite history file not found: {history_path}", file=sys.stderr)
        return 1

    runs = [json.loads(line) for line in history_path.read_text(encoding="utf-8").splitlines() if line.strip()]
    if not runs:
        print(f"suite history is empty: {history_path}", file=sys.stderr)
        return 1

    print(f"Suite history: {history_path}")
    print(f"Runs: {len(runs)}")

    totals_keys = ["duration_secs", "exec_steps", "apply_patch_steps", "plan_updates", "optional_confirmation_hits"]
    print("\nSuite bench totals")
    latest = runs[-1]["bench"]["totals"]
    for key in totals_keys:
        values = [run["bench"]["totals"].get(key, 0) for run in runs]
        print(
            f"- {key}: latest={values[-1]}, mean={statistics.mean(values):.2f}, min={min(values)}, max={max(values)}"
        )

    health_statuses = ["PASS" if "status: PASS" in run.get("health", "") else "FAIL" for run in runs]
    print("\nHealth")
    print(f"- pass_rate: {sum(1 for item in health_statuses if item == 'PASS')}/{len(health_statuses)}")
    print(f"- latest: {health_statuses[-1]}")

    print("\nScenario trend summary")
    scenario_names = sorted({scenario["name"] for run in runs for scenario in run["bench"].get("scenarios", [])})
    for name in scenario_names:
        durations = []
        exec_steps = []
        for run in runs:
            for scenario in run["bench"].get("scenarios", []):
                if scenario["name"] == name:
                    durations.append(scenario.get("duration_secs", 0))
                    exec_steps.append(scenario.get("metrics", {}).get("exec_steps", 0))
                    break
        print(
            f"- {name}: duration latest={durations[-1]} avg={statistics.mean(durations):.2f}, exec latest={exec_steps[-1]} avg={statistics.mean(exec_steps):.2f}"
        )

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
