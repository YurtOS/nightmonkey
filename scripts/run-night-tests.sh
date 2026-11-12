#!/usr/bin/env bash
# Run NightMonkey's own regression tests (tests/jit-test/**/*.js, plain
# scripts in the jit-test style) through the shell, in the AOT lane and,
# with NIGHT_INPROCESS_OFF=1, the interpreter lane. A test passes when the shell
# exits 0. For a build configured for engine <version> (NIGHT_ENGINE_VERSION in
# its CMakeCache.txt), tests/night-tests-excludes-<version>.txt, where present,
# lists the tests (paths relative to tests/jit-test, # comments) that engine
# cannot pass, e.g. one that needs a hook only another engine has.
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
skips=$(mktemp)
trap 'rm -f "$tests" "$skips"' EXIT
version=
if [ -f "$build/CMakeCache.txt" ]; then
  version=$(sed -n 's/^NIGHT_ENGINE_VERSION:[A-Z]*=//p' "$build/CMakeCache.txt")
fi
if [ -n "$version" ] && [ -f "$here/tests/night-tests-excludes-$version.txt" ]; then
  tr -d '\r' < "$here/tests/night-tests-excludes-$version.txt" | sed 's/[[:space:]]*$//' | grep -v '^[[:space:]]*\(#\|$\)' > "$skips" || true
fi
find "$here/tests/jit-test" -name '*.js' -type f -print0 > "$tests"
if [ ! -s "$tests" ]; then
  echo "no regression tests found in $here/tests/jit-test" >&2
  exit 2
fi
fail=0
pass=0
skipped=0
while IFS= read -r -d '' t; do
  if grep -Fxq -- "${t#"$here"/tests/jit-test/}" "$skips"; then
    skipped=$((skipped + 1))
    continue
  fi
  if out=$("$build/bin/inproc-shell.sh" "$t" 2>&1); then
    pass=$((pass + 1))
  else
    fail=$((fail + 1))
    echo "FAIL ${t#$here/}"
    printf '%s\n' "$out"
  fi
done < "$tests"
if [ "$skipped" -gt 0 ]; then
  echo "night tests: $pass passed, $fail failed, $skipped skipped for engine $version"
else
  echo "night tests: $pass passed, $fail failed"
fi
[ "$fail" -eq 0 ]
