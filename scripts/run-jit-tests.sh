#!/usr/bin/env bash
# Run SpiderMonkey's jit-tests against the NightMonkey shell.
#
#   scripts/run-jit-tests.sh <firefox-checkout> [build-dir] [-- jit_test.py args...]
#
# Both lanes skip tests/wasi-jit-test-excludes.txt (tests the wasm32-wasi
# shell cannot run at all). The AOT lane (default) compiles every test
# in-process and also skips tests/jit-test-excludes.txt; NIGHT_INPROCESS_OFF=1
# runs the same shell with the tier off (the baseline lane).
# Automation output is required to verify a nonempty executed suite.
set -euo pipefail

if [ $# -lt 1 ]; then
  echo "usage: $0 <firefox-checkout> [build-dir] [-- jit_test.py args...]" >&2
  exit 2
fi
firefox=$(cd "$1" && pwd)
shift
here=$(cd "$(dirname "$0")/.." && pwd)
build=$here/build
if [ $# -ge 1 ] && [ "$1" != "--" ]; then
  build=$(cd "$1" && pwd)
  shift
fi
if [ $# -ge 1 ] && [ "$1" = "--" ]; then
  shift
fi

# jit_test.py takes one --exclude-from; combine the lists.
work=$(mktemp -d)
combined=$work/excludes
log=$work/output
trap 'rm -rf "$work"' EXIT
cat "$here/tests/wasi-jit-test-excludes.txt" > "$combined"
if [ "${NIGHT_INPROCESS_OFF:-0}" != 1 ]; then
  cat "$here/tests/jit-test-excludes.txt" >> "$combined"
fi

python3 "$firefox/js/src/jit-test/jit_test.py" \
  --format=automation \
  --exclude-from "$combined" \
  "$build/bin/inproc-shell.sh" "$@" | tee "$log"
if ! grep -Eq '^Passed: [1-9][0-9]*$' "$log"; then
  echo "no jit-tests executed: missing positive test total" >&2
  exit 2
fi
