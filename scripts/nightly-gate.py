"""Decide whether a nightly workflow run needs to test anything.

    nightly-gate.py --event EVENT --head SHA --repo OWNER/NAME \
        --workflow FILE --branch BRANCH

Writes run=true or run=false to $GITHUB_OUTPUT (stdout when unset). A
scheduled run is skipped when the last completed scheduled run of the same
workflow on BRANCH (any conclusion) was for HEAD: nothing changed since.
Any other event always runs. When the runs API cannot be read the run goes
ahead with a warning: an outage must not silently stop the testing.
"""

import argparse
import json
import os
import subprocess
import sys


def decide(args):
    if args.event != "schedule":
        return True, f"{args.event}: always runs"
    path = (f"repos/{args.repo}/actions/workflows/{args.workflow}/runs"
            f"?event=schedule&status=completed&branch={args.branch}&per_page=1")
    try:
        result = subprocess.run(["gh", "api", path], capture_output=True, text=True,
                                check=True, timeout=60)
        runs = json.loads(result.stdout)["workflow_runs"]
        last = runs[0]["head_sha"] if runs else None
    except (OSError, subprocess.SubprocessError, ValueError, KeyError, TypeError, IndexError) as error:
        detail = getattr(error, "stderr", "") or error
        print(f"::warning::cannot read the previous scheduled runs ({detail}); running")
        return True, "previous run unknown"
    if last is None:
        return True, "no previous completed scheduled run"
    if last == args.head:
        return False, f"{args.branch} is still at {args.head}, tested by the last scheduled run"
    return True, f"{args.branch} moved from {last} to {args.head}"


def main():
    parser = argparse.ArgumentParser()
    for name in ("event", "head", "repo", "workflow", "branch"):
        parser.add_argument(f"--{name}", required=True)
    args = parser.parse_args()
    run, reason = decide(args)
    print(f"nightly gate: run={str(run).lower()}: {reason}")
    line = f"run={str(run).lower()}\n"
    target = os.environ.get("GITHUB_OUTPUT")
    if target:
        with open(target, "a", encoding="utf-8") as out:
            out.write(line)
    else:
        sys.stdout.write(line)
    return 0


if __name__ == "__main__":
    sys.exit(main())
