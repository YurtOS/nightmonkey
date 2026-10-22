#!/usr/bin/env bash
# Check that CI builds the engine from the YurtOS/yurt-firefox patch queue at a
# pinned commit (engine/QUEUE_PIN), not from a branch of the frozen fork.
#
#   scripts/check-engine-source.sh
set -euo pipefail

here=$(cd "$(dirname "$0")/.." && pwd)
workflow="$here/.github/workflows/engine.yml"
pin_file="$here/engine/QUEUE_PIN"
fail=0

err() { echo "check-engine-source: $*" >&2; fail=1; }

if grep -nE 'FIREFOX_REF|FIREFOX_REPO|YurtOS/firefox([^A-Za-z0-9_-]|$)' "$workflow" >&2; then
  err "engine.yml still references the YurtOS/firefox fork (see lines above)"
fi

grep -q 'repository: YurtOS/yurt-firefox' "$workflow" ||
  err "engine.yml does not check out YurtOS/yurt-firefox"
grep -q 'engine/QUEUE_PIN' "$workflow" ||
  err "engine.yml does not read engine/QUEUE_PIN"
grep -q 'scripts/build-engine.sh' "$workflow" ||
  err "engine.yml does not build with yurt-firefox's scripts/build-engine.sh"

if [ ! -f "$pin_file" ]; then
  err "engine/QUEUE_PIN is missing"
elif [ "$(wc -l <"$pin_file")" -ne 1 ] || ! grep -qE '^[0-9a-f]{40}$' "$pin_file"; then
  err "engine/QUEUE_PIN must be one line holding a 40-hex commit SHA"
fi

[ "$fail" -eq 0 ] && echo "check-engine-source: ok"
exit "$fail"
