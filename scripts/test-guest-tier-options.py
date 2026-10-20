#!/usr/bin/env python3
"""Execute the guest compiler's options contract against a built shell."""

import argparse
import json
import os
from pathlib import Path
import re
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("build", type=Path)
    args = parser.parse_args()
    build = args.build.resolve()
    wrapper = build / "bin/inproc-shell.sh"
    fixture = Path(__file__).resolve().parent.parent / "tests/jit-test/night/guest-tier-options.js"
    if not wrapper.is_file():
        parser.error(f"missing built wrapper: {wrapper}")
    env = os.environ.copy()
    env.pop("NIGHT_INPROCESS_OFF", None)
    env.pop("NIGHT_OPTIONS", None)
    env.update(NIGHT_INPROC_STRICT="1", NIGHT_INPROCESS_STATS="1",
               NIGHT_CACHE_DIR=str(build / "tier-options-cache"))
    for tier in ("mir", "baseline"):
        flags = f"--pipeline {tier} --strict-coverage --dump-tiers"
        result = subprocess.run(
            [str(wrapper), "--night-options", flags, str(fixture)],
            env=env, capture_output=True, text=True, timeout=600,
        )
        assert result.returncode == 0, result.stdout + result.stderr
        assert result.stdout.strip() == "42", result.stdout
        assert re.search(rf"night: tier \d+ {tier}\b", result.stderr), result.stderr
        assert re.search(r"\([1-9]\d* armed\)", result.stderr), result.stderr
        print(f"PASS guest {tier}: result, tier witness and arming")
    result = subprocess.run(
        [str(wrapper), "--night-options", "--pipeline invalid-tier", str(fixture)],
        env=env, capture_output=True, text=True, timeout=600,
    )
    assert result.returncode != 0, "invalid options silently succeeded"
    assert "night: inprocess:" in result.stderr, result.stderr
    assert "42" not in result.stdout.splitlines(), "invalid options ran interpreted"
    print("PASS invalid guest options: strict failure before execution")
    # The block census counts reach the dump: the flat per-block counter is
    # keyed on the kind the compiler emits (98), and the dump runs while the
    # context still holds the counts. Zero kind-98 lines would mean either broke.
    result = subprocess.run(
        [str(wrapper), "--night-options", "--pipeline mir --block-census", str(fixture)],
        env=env, capture_output=True, text=True, timeout=600,
    )
    assert result.returncode == 0, result.stdout + result.stderr
    counted = re.findall(r"^night: census kind 98 id \d+ n ([1-9]\d*)$", result.stderr, re.M)
    assert counted, "no kind-98 block census counts were printed:\n" + result.stderr[-2000:]
    print(f"PASS block census: {len(counted)} blocks counted")

    # A callee with direct eval is never inlined: the eval reads the caller's
    # interpreter frame, which an inlined callee does not have. `caller` has no
    # eval of its own, so any `call.eval` in its MIR came from inlining.
    eval_fixture = fixture.with_name("mir-eval-inline.js")
    viz = build / "eval-inline.viz"
    viz.unlink(missing_ok=True)
    result = subprocess.run(
        [str(wrapper), "--night-options", f"--pipeline mir --viz {viz}", str(eval_fixture)],
        env=env, capture_output=True, text=True, timeout=600,
    )
    assert result.returncode == 0, result.stdout + result.stderr
    seen = False
    for line in viz.read_text().splitlines():
        if not line.startswith("{"):
            continue
        func = json.loads(line)
        if func.get("name") == "caller":
            seen = True
            assert "call.eval" not in func["mir"], "an eval callee was inlined into caller"
    assert seen, "caller was not compiled by MIR"
    print("PASS direct eval is not inlined")


if __name__ == "__main__":
    main()
