#!/usr/bin/env python3
"""Benchmark gate: parses BENCH {"name":..., "value":..., "unit":...} lines from test output
and fails when a metric is missing or over its ceiling in bench/baselines.json.

Usage:
    python3 scripts/bench-check.py bench/baselines.json results1.log [results2.log ...]
    python3 scripts/bench-check.py --write bench/baselines.json results.log

Ceilings are microseconds and deliberately generous: shared CI runners are noisy, so the
gate catches order-of-magnitude regressions rather than jitter. After --write, the recorded
ceilings are the measured values multiplied by a headroom factor; review the diff.
"""
import json
import re
import sys

HEADROOM = 20.0

BENCH = re.compile(r'^BENCH (\{.*\})\s*$')


def measure(paths):
    found = {}
    for path in paths:
        for line in open(path, encoding="utf-8"):
            match = BENCH.match(line.strip())
            if match:
                entry = json.loads(match.group(1))
                assert entry["unit"] == "us", f"unexpected unit in {entry}"
                found[entry["name"]] = entry["value"]
    return found


def write(path, measured):
    ceilings = {
        "_note": "microsecond ceilings for the bench gate; regenerated with --write",
        **{name: int(value * HEADROOM) for name, value in sorted(measured.items())},
    }
    with open(path, "w", encoding="utf-8") as out:
        json.dump(ceilings, out, indent=2, sort_keys=True)
        out.write("\n")


def main(argv):
    if argv[0] == "--write":
        baseline, logs = argv[1], argv[2:]
        measured = measure(logs)
        if not measured:
            print("no BENCH lines found in", logs)
            return 2
        write(baseline, measured)
        print(f"wrote {len(measured)} ceilings to {baseline}")
        return 0

    baseline, logs = argv[0], argv[1:]
    with open(baseline, encoding="utf-8") as source:
        ceilings = {
            name: ceiling
            for name, ceiling in json.load(source).items()
            if not name.startswith("_")
        }
    measured = measure(logs)

    failures = []
    print(f"{'metric':<38} {'measured':>12} {'ceiling':>12}")
    for name, ceiling in sorted(ceilings.items()):
        value = measured.get(name)
        if value is None:
            failures.append(f"{name}: not found in the results")
            print(f"{name:<38} {'missing':>12} {ceiling:>12.0f}")
            continue
        over = value > ceiling
        if over:
            failures.append(f"{name}: {value:.0f}us over ceiling {ceiling}us")
        print(f"{name:<38} {value:>12.0f} {ceiling:>12.0f} {'OVER' if over else ''}")
    unexpected = set(measured) - set(ceilings)
    if unexpected:
        failures.append(f"metrics with no ceiling: {sorted(unexpected)}")

    for failure in failures:
        print(f"bench gate: {failure}", file=sys.stderr)
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
