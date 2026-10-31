#!/usr/bin/env bash
# Pin a YurtOS/yurt-firefox engine release: write engine/ENGINE_RELEASE.
#
#   scripts/update-engine-release.sh <tag>      (e.g. engine-v147.0.4-1)
#
# Downloads the release's MANIFEST.json and every asset's .sha256 file with
# `gh release download`, requires the manifest's release.tag to be <tag>, its
# release.assets to list the five assets, and each digest to equal the one in
# the asset's own .sha256 line; then writes ENGINE_RELEASE with the tag, the
# sha256 of MANIFEST.json and each asset's digest. Nothing is written if any
# check fails. Review the diff: the digests are what CI will trust.
#
# The rest of an engine bump (see README "Engine release"): if the engine's
# version changed, regenerate the opcode table with scripts/gen_opcodes.py
# from the release's include-private/vm/Opcodes.h, and run the engine jobs.
# Exit codes: 0 written; 1 refused; 2 usage.
set -euo pipefail
unset CDPATH

REPO=YurtOS/yurt-firefox
here=$(cd "$(dirname "$0")/.." && pwd)

if [ $# -ne 1 ] || ! [[ $1 =~ ^engine-v[0-9]+(\.[0-9]+)+-[1-9][0-9]*$ ]]; then
  echo "usage: $0 engine-v<version>-<n>" >&2
  exit 2
fi
tag=$1
base=spidermonkey_${tag#engine-v}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

get() {
  gh release download "$tag" --repo "$REPO" --dir "$work" --pattern "$1" ||
    { echo "update-engine-release: $1: download of $tag failed" >&2; exit 1; }
}
get MANIFEST.json
for k in wasm32-wasi testsrc jstests toolchain-clang toolchain-sysroot; do
  get "$base-$k.tar.zst.sha256"
done

python3 -I - "$work" "$tag" "$base" >"$work/ENGINE_RELEASE" <<'PY' || exit 1
import hashlib, json, os, re, sys
work, tag, base = sys.argv[1:]
raw = open(os.path.join(work, "MANIFEST.json"), "rb").read()
m = json.loads(raw)
r = m.get("release") or {}
if r.get("tag") != tag:
    sys.exit("update-engine-release: MANIFEST.json: release.tag %r is not %s" % (r.get("tag"), tag))
assets = {a.get("name"): a.get("sha256") for a in r.get("assets", [])}
lines = ["# The YurtOS/yurt-firefox engine release CI builds against",
         "# (engine-release.yml). Written by scripts/update-engine-release.sh;",
         "# checked by scripts/check-engine-release.sh.",
         "tag=" + tag, "manifest=" + hashlib.sha256(raw).hexdigest()]
for kind in ("wasm32-wasi", "testsrc", "jstests", "toolchain-clang", "toolchain-sysroot"):
    name = "%s-%s.tar.zst" % (base, kind)
    digest = assets.get(name)
    if not isinstance(digest, str) or not re.fullmatch(r"[0-9a-f]{64}", digest):
        sys.exit("update-engine-release: MANIFEST.json: no sha256 for %s (%s)" % (name, kind))
    line = open(os.path.join(work, name + ".sha256")).read()
    if line != "%s  %s\n" % (digest, name):
        sys.exit("update-engine-release: %s.sha256 does not agree with MANIFEST.json (%s)" % (name, digest))
    lines.append("%s=%s" % (kind, digest))
print("\n".join(lines))
PY
cp "$work/ENGINE_RELEASE" "$here/engine/ENGINE_RELEASE"
echo "update-engine-release: wrote engine/ENGINE_RELEASE for $tag"
echo "update-engine-release: if the engine version changed, regenerate the opcode table: scripts/gen_opcodes.py generate <version> <spidermonkey>/include-private/vm/Opcodes.h"
