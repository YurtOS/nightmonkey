#!/usr/bin/env bash
# Run SpiderMonkey's jstests (the test262 and non262 suites) against the
# NightMonkey shell.
#
#   scripts/run-jstests.sh <firefox-checkout> [build-dir] [-- jstests.py args...]
#
# Both lanes skip tests/wasi-jstests-excludes.txt (tests the wasm32-wasi
# shell cannot run at all). The AOT lane (default) also skips
# tests/jstests-excludes.txt; NIGHT_INPROCESS_OFF=1 runs the tier-off
# baseline lane. Each list may have a tests/<list>-<engine version>.txt
# companion for that engine.
# The full suite takes hours; pass a path filter to narrow it.
# Automation output is required to verify actual passing execution results.
set -euo pipefail

if [ $# -lt 1 ]; then
  echo "usage: $0 <firefox-checkout> [build-dir] [-- jstests.py args...]" >&2
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

# The engine version the build is configured for (NIGHT_ENGINE_VERSION);
# tests/<list>-<version>.txt, where present, adds that version's exclusions
# (a test only that engine fails, so that the others keep their coverage), as
# run-jit-tests.sh does.
version=
if [ -f "$build/CMakeCache.txt" ]; then
  version=$(sed -n 's/^NIGHT_ENGINE_VERSION:[A-Z]*=//p' "$build/CMakeCache.txt")
fi
add_list() {
  excludes+=(--exclude-file "$here/tests/$1.txt")
  if [ -n "$version" ] && [ -f "$here/tests/$1-$version.txt" ]; then
    excludes+=(--exclude-file "$here/tests/$1-$version.txt")
  fi
}
excludes=()
add_list wasi-jstests-excludes
if [ "${NIGHT_INPROCESS_OFF:-0}" != 1 ]; then
  add_list jstests-excludes
fi

# jstests.py reads TARGET_XPCOM_ABI, OS_TARGET and MOZ_DEBUG from a
# config/autoconf.mk above the shell to evaluate manifest conditions
# (winWidget, Android, is64Bit, isDebugBuild); without one it exits before
# running anything. The shell here is build/bin, outside any objdir, so pass
# them. An engine release's test sources (NIGHT_FIREFOX_TESTS,
# engine-release.yml) carry the value read from the objdir that built the
# engine, in xul-info.txt at their root: that one wins. It must be one line,
# ABI:OS:DEBUG. The default, for a Firefox checkout, is the value the
# release records (engine-v147.0.4-1, same for engine-v159.0-1): wasm32 target (toolchain.configure's
# "<cpu>-gcc3"), OS_TARGET WASI, not a debug build.
xul_info=wasm32-gcc3:WASI:false
if [ -e "$firefox/xul-info.txt" ]; then
  xul_info=$(cat "$firefox/xul-info.txt")
  if [ "$(wc -l < "$firefox/xul-info.txt")" -ne 1 ] || ! [[ "$xul_info" =~ ^[^:[:space:]]+:[^:[:space:]]+:(true|false)$ ]]; then
    echo "run-jstests: $firefox/xul-info.txt must be one line ABI:OS:true|false, got '$xul_info'" >&2
    exit 2
  fi
fi

log=$(mktemp)
trap 'rm -f "$log"' EXIT
python3 -u "$here/scripts/run-jstests-checked.py" "$firefox/js/src/tests/jstests.py" \
  --format=automation \
  --xul-info="$xul_info" \
  "${excludes[@]}" \
  --wpt=disabled \
  "$build/bin/inproc-shell.sh" "$@" 2>&1 | tee "$log"
python3 "$here/scripts/check-test-log.py" jstests "$log"
