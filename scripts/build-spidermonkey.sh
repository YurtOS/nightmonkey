#!/usr/bin/env bash
# Rebuild SpiderMonkey for NightMonkey (the wasm32-wasi JS shell with the
# external compiler hook surface, spidermonkey/mozconfig) in an existing patched
# tree. The tree comes from YurtOS/yurt-firefox at the commit in
# engine/QUEUE_PIN: its scripts/build-engine.sh fetches the pinned Firefox tag
# and applies the patch queue (--dry-run stops there; without it, it also
# bootstraps the toolchains and builds, as CI does, and this script is only
# needed to rebuild after local edits). CI fails unless spidermonkey/mozconfig
# is byte-identical to yurt-firefox's mozconfig at that commit.
#
#   scripts/build-spidermonkey.sh <patched-firefox-tree>
#
# Produces <patched-firefox-tree>/obj-nightmonkey-sm/dist, the SPIDERMONKEY_DIST
# the CMake build consumes.
set -euo pipefail

if [ $# -ne 1 ]; then
  echo "usage: $0 <patched-firefox-tree>" >&2
  exit 2
fi
firefox=$(cd "$1" && pwd)
here=$(cd "$(dirname "$0")/.." && pwd)

cd "$firefox"
MOZCONFIG="$here/spidermonkey/mozconfig" ./mach build
echo "SPIDERMONKEY_DIST=$firefox/obj-nightmonkey-sm/dist"
