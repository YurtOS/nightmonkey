#!/usr/bin/env bash
# Run NightMonkey's own regression tests (tests/jit-test/**/*.js, plain
# scripts in the jit-test style) through the shell, in the AOT lane and,
# with NIGHT_INPROCESS_OFF=1, the interpreter lane. A test passes when the shell
# exits 0.
#
#   scripts/run-night-tests.sh [build-dir]
set -euo pipefail

here=$(cd "$(dirname "$0")/.." && pwd)
build=${1:-$here/build}
if [ ! -x "$build/bin/inproc-shell.sh" ]; then
  echo "missing executable shell wrapper: $build/bin/inproc-shell.sh" >&2
  exit 2
fi
if [ ! -d "$here/tests/jit-test" ]; then
  echo "missing regression test directory: $here/tests/jit-test" >&2
  exit 2
fi
tests=$(mktemp)
trap 'rm -f "$tests"' EXIT
find "$here/tests/jit-test" -name '*.js' -type f -print0 > "$tests"
if [ ! -s "$tests" ]; then
  echo "no regression tests found in $here/tests/jit-test" >&2
  exit 2
fi
fail=0
pass=0
while IFS= read -r -d '' t; do
  if out=$("$build/bin/inproc-shell.sh" "$t" 2>&1); then
    pass=$((pass + 1))
  else
    fail=$((fail + 1))
    echo "FAIL ${t#$here/}"
    printf '%s\n' "$out"
  fi
done < "$tests"
echo "night tests: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
