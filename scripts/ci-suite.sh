#!/usr/bin/env bash
# CI lane policy: run every requested lane, retaining each lane's status/log.
#   scripts/ci-suite.sh <local|jit|jstests> <aot|interpreter|both>
# Requires GNU timeout (provided by Ubuntu coreutils).
set -euo pipefail

if [ "$#" -ne 2 ]; then
  echo "usage: $0 <local|jit|jstests> <aot|interpreter|both>" >&2
  exit 2
fi
suite=$1
here=$(cd "$(dirname "$0")/.." && pwd)
case "$suite" in
  local) seconds=1800; harness=("$here/scripts/run-night-tests.sh" "$here/build") ;;
  jit) seconds=3600; harness=("$here/scripts/run-jit-tests.sh" "$here/firefox" "$here/build") ;;
  jstests) seconds=7200; harness=("$here/scripts/run-jstests.sh" "$here/firefox" "$here/build") ;;
  *) echo "unknown suite: $suite" >&2; exit 2 ;;
esac
case "$2" in
  aot|interpreter) lanes=("$2") ;;
  both) lanes=(aot interpreter) ;;
  *) echo "unknown lane: $2" >&2; exit 2 ;;
esac

logs=${RUNNER_TEMP:?RUNNER_TEMP must name the CI temporary directory}/nightmonkey-logs
mkdir -p "$logs"
failed=0
for lane in "${lanes[@]}"; do
  off=0
  strict=0
  opts=
  if [ "$lane" = interpreter ]; then off=1; else strict=1; fi
  # The compiled local and jit lanes also fail a script left interpreted for a
  # reason that is not a designed-out op, or a tier that hit a compiler bug
  # (`BUG:`): a MIR bug falls back to baseline, which passes any test that
  # checks results only. Verified on the full jit-test suite; the jstests lane
  # has not been run this way, so it does not get it.
  if [ "$strict" = 1 ] && [ "$suite" != jstests ]; then opts=--strict-coverage; fi
  # A compiled lane runs strict: a failed batch must fail the run, not leave
  # the scripts interpreted and every test green.
  if timeout --kill-after=30s "${seconds}s" \
      env NIGHT_INPROCESS_OFF="$off" NIGHT_INPROC_STRICT="$strict" NIGHT_OPTIONS="$opts" \
      "${harness[@]}" 2>&1 | tee "$logs/$suite-$lane.log"; then
    echo "$suite ($lane): passed"
  else
    status=$?
    echo "$suite ($lane): failed with status $status" >&2
    failed=1
  fi
done
exit "$failed"
