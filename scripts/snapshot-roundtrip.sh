#!/usr/bin/env bash
# Snapshot round-trip: the shipping snapshot flow on a representative main().
#   scripts/snapshot-roundtrip.sh [<build-dir> [<program.js> <expected-stdout>]]
# Defaults: <repo>/build and tests/fixtures/snapshot-roundtrip/{main.js,expected.txt}.
# `nightmonkey --shell <build>/bin/js` wizens the program (its top level runs
# into the image), keeps the raw snapshot and writes the AOT module; both are
# restored with `wasmtime run`, which calls main(). Each restore must exit 0
# and print exactly the expected stdout.
# The runner is $WASMTIME, else `wasmtime` on PATH, else $HOME/bin/wasmtime.
# Exit 0 pass; 1 round-trip failure; 2 usage or missing prerequisite.
set -euo pipefail

usage() {
  echo "usage: $0 [<build-dir> [<program.js> <expected-stdout>]]" >&2
  exit 2
}
setup() {
  echo "snapshot-roundtrip: $*" >&2
  exit 2
}
fail() {
  echo "snapshot-roundtrip: FAIL: $*" >&2
  exit 1
}

here=$(cd "$(dirname "$0")/.." && pwd)
case $# in
  0 | 1) program=$here/tests/fixtures/snapshot-roundtrip/main.js
         expected=$here/tests/fixtures/snapshot-roundtrip/expected.txt ;;
  3) program=$2; expected=$3 ;;
  *) usage ;;
esac
build=${1:-$here/build}

nightmonkey=$build/bin/nightmonkey
shell=$build/bin/js
[ -x "$nightmonkey" ] || setup "missing $nightmonkey (build NightMonkey first)"
[ -f "$shell" ] || setup "missing $shell (build NightMonkey first)"
[ -f "$program" ] || setup "missing program $program"
[ -f "$expected" ] || setup "missing expected output $expected"

if [ -n "${WASMTIME:-}" ]; then
  runner=$WASMTIME
  [ -x "$runner" ] || setup "WASMTIME=$runner is not an executable wasmtime"
elif runner=$(command -v wasmtime); then
  :
elif [ -x "${HOME:-}/bin/wasmtime" ]; then
  runner=$HOME/bin/wasmtime
else
  setup "no wasmtime: set WASMTIME, or put wasmtime on PATH or at \$HOME/bin/wasmtime"
fi

# nightmonkey wants absolute paths (AGENTS.md "Verification").
abs() { (cd "$(dirname "$1")" && printf '%s/%s\n' "$(pwd)" "$(basename "$1")"); }
shell=$(abs "$shell")
program=$(abs "$program")

work=$(mktemp -d "${TMPDIR:-/tmp}/snapshot-roundtrip.XXXXXX")
trap 'rm -rf "$work"' EXIT
snapshot=$work/snapshot.wasm
module=$work/program-aot.wasm

status=0
"$nightmonkey" --shell "$shell" "$program" --keep-snapshot "$snapshot" -o "$module" >&2 ||
  status=$?
[ "$status" -eq 0 ] || fail "nightmonkey failed (status $status) for $program"
[ -s "$snapshot" ] || fail "kept snapshot is empty or missing: $snapshot"
[ -s "$module" ] || fail "AOT module is empty or missing: $module"

restore() { # <label> <module>
  local out=$work/$1.stdout status=0
  "$runner" run "$2" > "$out" || status=$?
  [ "$status" -eq 0 ] || fail "restored $1 exited with status $status"
  if ! cmp -s "$expected" "$out"; then
    diff -u "$expected" "$out" >&2 || true
    fail "restored $1 output differs from $expected"
  fi
}
restore snapshot "$snapshot"
restore "AOT module" "$module"
echo "snapshot round-trip passed: $program"
