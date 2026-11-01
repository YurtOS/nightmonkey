#!/usr/bin/env bash
# Run SpiderMonkey's jit-tests against the NightMonkey shell.
#
#   scripts/run-jit-tests.sh <firefox-checkout> [build-dir] [-- jit_test.py args...]
#
# Both lanes skip tests/wasi-jit-test-excludes.txt (tests the wasm32-wasi
# shell cannot run at all). The AOT lane (default) compiles every test
# in-process and also skips tests/jit-test-excludes.txt; NIGHT_INPROCESS_OFF=1
# runs the same shell with the tier off (the interpreter lane). Each list
# may have a tests/<list>-<engine version>.txt companion for that engine.
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
# The engine version the build is configured for (NIGHT_ENGINE_VERSION);
# tests/<list>-<version>.txt, where present, adds that version's exclusions
# (a test the engine changed, so that it keeps its coverage on the others).
version=
if [ -f "$build/CMakeCache.txt" ]; then
  version=$(sed -n 's/^NIGHT_ENGINE_VERSION:[A-Z]*=//p' "$build/CMakeCache.txt")
fi
add_list() {
  cat "$here/tests/$1.txt" >> "$combined"
  if [ -n "$version" ] && [ -f "$here/tests/$1-$version.txt" ]; then
    cat "$here/tests/$1-$version.txt" >> "$combined"
  fi
}
: > "$combined"
add_list wasi-jit-test-excludes
if [ "${NIGHT_INPROCESS_OFF:-0}" != 1 ]; then
  add_list jit-test-excludes
fi

python3 -u "$firefox/js/src/jit-test/jit_test.py" \
  --format=automation \
  --exclude-from "$combined" \
  "$build/bin/inproc-shell.sh" "$@" 2>&1 | tee "$log"
python3 "$here/scripts/check-test-log.py" jit "$log"
