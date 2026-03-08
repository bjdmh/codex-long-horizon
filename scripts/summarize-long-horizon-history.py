#!/usr/bin/env python3
import json
import statistics
import sys
from pathlib import Path


def main() -> int:
    history_path = Path(sys.argv[1]) if len(sys.argv) > 1 else Path("/tmp/long-horizon-bench/history.jsonl")
    if not history_path.exists():
        print(f"history file not found: {history_path}", file=sys.stderr)
        return 1

    runs = [json.loads(line) for line in history_path.read_text(encoding="utf-8").splitlines() if line.strip()]
    if not runs:
        print(f"history file is empty: {history_path}", file=sys.stderr)
        return 1

    print(f"History: {history_path}")
    print(f"Runs: {len(runs)}")

    total_keys = ["duration_secs", "exec_steps", "apply_patch_steps", "plan_updates", "optional_confirmation_hits"]
    print("\nAggregate totals")
    for key in total_keys:
        values = [run.get("totals", {}).get(key, 0) for run in runs]
        latest = values[-1]
        mean = statistics.mean(values)
        minimum = min(values)
        maximum = max(values)
        print(f"- {key}: latest={latest}, mean={mean:.2f}, min={minimum}, max={maximum}")

    print("\nScenario trends")
    scenario_names = sorted({scenario["name"] for run in runs for scenario in run.get("scenarios", [])})
    for name in scenario_names:
        durations = []
        exec_steps = []
        confirmations = []
        for run in runs:
            for scenario in run.get("scenarios", []):
                if scenario["name"] == name:
                    durations.append(scenario.get("duration_secs", 0))
                    metrics = scenario.get("metrics", {})
                    exec_steps.append(metrics.get("exec_steps", 0))
                    confirmations.append(metrics.get("optional_confirmation_hits", 0))
                    break

        latest_duration = durations[-1]
        latest_exec = exec_steps[-1]
        latest_confirm = confirmations[-1]
        print(
            f"- {name}: duration latest={latest_duration}s avg={statistics.mean(durations):.2f}s, "
            f"exec latest={latest_exec} avg={statistics.mean(exec_steps):.2f}, "
            f"confirm latest={latest_confirm} total={sum(confirmations)}"
        )

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
