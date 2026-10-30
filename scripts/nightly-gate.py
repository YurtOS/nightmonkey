"""Decide whether a nightly workflow run needs to test anything.

    nightly-gate.py --event EVENT --head SHA --repo OWNER/NAME \
        --workflow FILE --branch BRANCH

Writes run=true or run=false to $GITHUB_OUTPUT (stdout when unset). The
base is the newest completed run of the same workflow on BRANCH started by
a schedule or a workflow_dispatch (so a green manual retry clears a red).
A scheduled run whose HEAD is the base's head repeats the base's result in
seconds: after a green base it skips (run=false); after a red one it exits 1
with an error naming that run, so the red stays visible without a rebuild.
Any other base conclusion (cancelled, timed out...), no base, an unreadable
runs API, or a moved HEAD runs the suites; so does every other event.
"""

import argparse
import json
import os
import subprocess
import sys


BASE_EVENTS = ("schedule", "workflow_dispatch")


def decide(args):
    if args.event != "schedule":
        return True, f"{args.event}: always runs"
    path = (f"repos/{args.repo}/actions/workflows/{args.workflow}/runs"
            f"?status=completed&branch={args.branch}&per_page=50")
    try:
        result = subprocess.run(["gh", "api", path], capture_output=True, text=True,
                                check=True, timeout=60)
        runs = [r for r in json.loads(result.stdout)["workflow_runs"]
                if r["event"] in BASE_EVENTS and r["head_branch"] == args.branch]
        base = max(runs, key=lambda r: r["created_at"]) if runs else None
        last = base["head_sha"] if base else None
    except (OSError, subprocess.SubprocessError, ValueError, KeyError, TypeError, IndexError) as error:
        detail = getattr(error, "stderr", "") or error
        print(f"::warning::cannot read the previous scheduled runs ({detail}); running")
        return True, "previous run unknown"
    if last is None:
        return True, "no previous completed scheduled or dispatched run"
    if last != args.head:
        return True, f"{args.branch} moved from {last} to {args.head}"
    url = base.get("html_url", "the last scheduled run")
    conclusion = base.get("conclusion")
    if conclusion == "success":
        return False, f"{args.branch} is still at {args.head}, green in {url}"
    if conclusion == "failure":
        print(f"::error::nothing changed since the red run {url} ({args.branch} is still at "
              f"{args.head}); dispatch the workflow to retry")
        sys.exit(1)
    return True, f"{args.branch} is still at {args.head}, but {url} ended {conclusion}"


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
