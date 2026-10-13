#!/usr/bin/env python3
"""Run write-test over a project's untested unit obligations, and record how it did.

Usage: scripts/eval-write-test.py <project> <results.jsonl> [--thinking always|never|fallback] [--only id,id,...]

One JSON line per obligation: outcome (verified, gave_up, gave_up_maybe_unreachable, error),
attempts, seconds, the last problem and the thinking mode. Obligations already in the results
file are skipped, so an interrupted run continues where it stopped. Nothing is added to the
project: write-test runs without a terminal and so never applies. Transcripts go next to the
results file.
"""
import json
import os
import re
import subprocess
import sys
import time

AGENT = os.path.join(os.path.dirname(__file__), "..", "target", "release", "skilj-agent")


def untested(project):
    report = json.loads(subprocess.run([AGENT, "check", project, "--json"], capture_output=True, check=True, text=True).stdout)
    return [
        o["id"]
        for o in report["obligations"]
        if o["source"]["type"] == "code_index" and o["level"] == "unit" and not o["covered_by"]
    ]


def outcome(output):
    verified = re.search(r"^Verified after (\d+) attempt", output, re.M)
    if verified:
        return "verified", int(verified.group(1)), None
    gave_up = re.search(r"^Gave up after (\d+) attempts; the last one: (.*)$", output, re.M)
    if gave_up:
        kind = "gave_up_maybe_unreachable" if "may be unreachable" in output else "gave_up"
        return kind, int(gave_up.group(1)), gave_up.group(2)[:300]
    last = [l for l in output.strip().splitlines() if l.strip()][-1:] or [""]
    return "error", 0, last[0][:300]


def main():
    args = sys.argv[1:]
    if len(args) < 2:
        sys.exit(__doc__)
    project, results = args[0], args[1]
    thinking = "never" if "--no-think" in args else None
    if "--thinking" in args:
        thinking = args[args.index("--thinking") + 1]
    only = None
    if "--only" in args:
        only = args[args.index("--only") + 1].split(",")

    done = set()
    if os.path.exists(results):
        with open(results) as f:
            done = {json.loads(l)["obligation"] for l in f if l.strip()}
    transcripts = os.path.splitext(results)[0] + "-transcripts"
    os.makedirs(transcripts, exist_ok=True)

    todo = [o for o in (only or untested(project)) if o not in done]
    print(f"{len(todo)} to run, {len(done)} already done", file=sys.stderr)
    for n, obligation in enumerate(todo, 1):
        cmd = [AGENT, "write-test", obligation, "--path", project,
               "--transcript", os.path.join(transcripts, obligation + ".md")]
        if thinking:
            cmd += ["--thinking", thinking]
        start = time.time()
        run = subprocess.run(cmd, stdin=subprocess.DEVNULL, capture_output=True, text=True)
        took = round(time.time() - start)
        kind, attempts, problem = outcome(run.stdout + run.stderr)
        line = {"obligation": obligation, "outcome": kind, "attempts": attempts, "seconds": took,
                "problem": problem, "thinking": thinking or "default"}
        with open(results, "a") as f:
            f.write(json.dumps(line) + "\n")
        print(f"[{n}/{len(todo)}] {obligation}: {kind} ({attempts} attempts, {took}s)", file=sys.stderr, flush=True)


if __name__ == "__main__":
    main()
