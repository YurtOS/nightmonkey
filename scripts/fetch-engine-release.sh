#!/usr/bin/env bash
# Download, verify and extract the YurtOS/yurt-firefox engine release pinned in
# engine/ENGINE_RELEASE (engine-release.yml's fetch step).
#
#   scripts/fetch-engine-release.sh [--release FILE] --jstests true|false <dest>
#
# ENGINE_RELEASE holds `tag=engine-v<version>-<n>` and the sha256 of
# MANIFEST.json (`manifest=`) and of each asset spidermonkey_<version>-<n>-
# {wasm32-wasi,testsrc,jstests,toolchain-clang,toolchain-sysroot}.tar.zst
# (`<kind>=`). Each needed asset is downloaded with `gh release download`
# (the repository is public; in Actions gh still wants GH_TOKEN, the job's
# read-only token), and EVERY digest is checked before anything is
# extracted: each file against ENGINE_RELEASE, and MANIFEST.json's
# release.tag and release.assets digests against ENGINE_RELEASE too. The
# jstests asset is downloaded and checked only with --jstests true.
#
# Then, into <dest> (CI: $RUNNER_TEMP):
#   spidermonkey/          the engine archive (its top directory), passed to
#                          CMake as SPIDERMONKEY_DIST
#   toolchain/clang/       NIGHT_TOOLCHAIN_ROOT (bin/clang, bin/clang++)
#   toolchain/sysroot-wasm32-wasi/  NIGHT_SYSROOT
#   testsrc/               the test-source bundle, plus the jstests bundle
#                          (and its xul-info.txt) with --jstests true:
#                          NIGHT_FIREFOX_TESTS
# Every archive is listed before extraction: members must be regular files,
# directories, or links whose target has no ".." and is not absolute, under
# the archive's expected top level (spidermonkey/, js/ and xul-info.txt,
# clang/, sysroot-wasm32-wasi/). Those directories must not exist yet.
# Prints, and appends to $GITHUB_OUTPUT when set: tag=, spidermonkey_dist=,
# toolchain_root=, sysroot=, testsrc=.
# Exit codes: 0 ok; 1 refused or failed (the message names the file); 2 usage.
set -euo pipefail
unset CDPATH

REPO=YurtOS/yurt-firefox
here=$(cd "$(dirname "$0")/.." && pwd)
release=$here/engine/ENGINE_RELEASE

usage() {
  [ $# -eq 0 ] || echo "fetch-engine-release: $*" >&2
  echo "usage: $0 [--release FILE] --jstests true|false <dest>" >&2
  exit 2
}
die() { echo "fetch-engine-release: $*" >&2; exit 1; }

jstests='' dest=''
while [ $# -gt 0 ]; do
  case $1 in
    --release) [ $# -ge 2 ] || usage "--release needs a value"; release=$2; shift 2 ;;
    --jstests) [ $# -ge 2 ] || usage "--jstests needs a value"; jstests=$2; shift 2 ;;
    -*) usage "unknown argument: $1" ;;
    *) [ -z "$dest" ] || usage "one <dest>"; dest=$1; shift ;;
  esac
done
case $jstests in true | false) ;; *) usage "--jstests must be true or false, got '$jstests'" ;; esac
[ -n "$dest" ] || usage "<dest> is required"
mkdir -p "$dest"
dest=$(cd "$dest" && pwd -P)
command -v zstd >/dev/null 2>&1 || die "zstd is required"
command -v gh >/dev/null 2>&1 || die "gh is required"

for d in spidermonkey toolchain testsrc engine-release-download; do
  if [ -e "$dest/$d" ]; then die "$dest/$d exists; refusing to mix releases"; fi
done

# ENGINE_RELEASE: exactly these keys, once each, in this order.
pins=$(mktemp)
trap 'rm -f "$pins"' EXIT
python3 -I - "$release" >"$pins" <<'PY' || exit 1
import re, sys
path = sys.argv[1]
keys = ["tag", "manifest", "wasm32-wasi", "testsrc", "jstests", "toolchain-clang", "toolchain-sysroot"]
try:
    text = open(path).read()
except OSError as e:
    sys.exit("fetch-engine-release: engine/ENGINE_RELEASE: %s" % e)
got = []
for line in text.splitlines():
    if not line or line.startswith("#"):
        continue
    k, sep, v = line.partition("=")
    if not sep:
        sys.exit("fetch-engine-release: engine/ENGINE_RELEASE: not key=value: %r" % line)
    got.append((k, v))
if [k for k, _ in got] != keys:
    sys.exit("fetch-engine-release: engine/ENGINE_RELEASE keys %s, want exactly %s" % ([k for k, _ in got], keys))
d = dict(got)
if not re.fullmatch(r"engine-v[0-9]+(\.[0-9]+)+-[1-9][0-9]*", d["tag"]):
    sys.exit("fetch-engine-release: engine/ENGINE_RELEASE: tag %r is not a fixed engine-v<version>-<n> (still PENDING?)" % d["tag"])
for k in keys[1:]:
    if not re.fullmatch(r"[0-9a-f]{64}", d[k]):
        sys.exit("fetch-engine-release: engine/ENGINE_RELEASE: %s=%r is not a sha256" % (k, d[k]))
for k in keys:
    print("%s=%s" % (k.replace("-", "_"), d[k]))
PY
tag=$(sed -n 's/^tag=//p' "$pins")
digest_of() { sed -n "s/^${1//-/_}=//p" "$pins"; }
base=spidermonkey_${tag#engine-v}

kinds="wasm32-wasi testsrc toolchain-clang toolchain-sysroot"
[ "$jstests" = false ] || kinds="$kinds jstests"

dl=$dest/engine-release-download
mkdir -p "$dl"
sha() { if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1"; else shasum -a 256 "$1"; fi | cut -d' ' -f1; }

# fetch <name> <sha256>: download one asset and check its digest.
fetch() {
  local name=$1 want=$2 got
  gh release download "$tag" --repo "$REPO" --dir "$dl" --pattern "$name" ||
    die "$name: download of $tag from $REPO failed"
  [ -f "$dl/$name" ] || die "$name: not downloaded"
  got=$(sha "$dl/$name")
  [ "$got" = "$want" ] || die "$name: sha256 $got is not ENGINE_RELEASE's $want"
  echo "fetch-engine-release: $name verified"
}

fetch MANIFEST.json "$(digest_of manifest)"
for k in $kinds; do
  fetch "$base-$k.tar.zst" "$(digest_of "$k")"
done

# The pinned manifest must describe this release with the same digests.
python3 -I - "$dl/MANIFEST.json" "$pins" "$tag" "$base" <<'PY' || exit 1
import json, sys
mpath, env, tag, base = sys.argv[1:]
pins = dict(l.strip().split("=", 1) for l in open(env) if "=" in l)
m = json.load(open(mpath))
r = m.get("release") or {}
if r.get("tag") != tag:
    sys.exit("fetch-engine-release: MANIFEST.json: release.tag %r is not %s" % (r.get("tag"), tag))
assets = {a.get("name"): a.get("sha256") for a in r.get("assets", [])}
for kind in ("wasm32-wasi", "testsrc", "jstests", "toolchain-clang", "toolchain-sysroot"):
    name = "%s-%s.tar.zst" % (base, kind)
    if assets.get(name) != pins[kind.replace("-", "_")]:
        sys.exit("fetch-engine-release: MANIFEST.json: %s is %s, ENGINE_RELEASE says %s"
                 % (name, assets.get(name), pins[kind.replace("-", "_")]))
PY

# check_members <archive> <top...>: list it; refuse anything but files,
# directories and links that stay under one of the tops; print the tops seen.
check_members() {
  local archive=$1
  shift
  zstd -q -dc "$archive" | python3 -I -c '
import posixpath, sys, tarfile
name_, tops = sys.argv[1], sys.argv[2:]
seen = set()
for m in tarfile.open(fileobj=sys.stdin.buffer, mode="r|"):
    parts = [x for x in m.name.split("/") if x not in ("", ".")]
    if not parts or m.name.startswith("/") or ".." in parts or parts[0] not in tops:
        sys.exit("%s: member %r is outside %s" % (name_, m.name, tops))
    if m.issym() or m.islnk():
        lp = m.linkname.split("/")
        if m.linkname.startswith("/") or ".." in lp:
            sys.exit("%s: link %r -> %r may leave the archive" % (name_, m.name, m.linkname))
    elif not (m.isfile() or m.isdir()):
        sys.exit("%s: member %r is not a file, directory or link" % (name_, m.name))
    seen.add(parts[0])
print(" ".join(sorted(seen)))
' "${archive##*/}" "$@"
}

for k in $kinds; do
  case $k in
    wasm32-wasi) tops=spidermonkey ;;
    testsrc) tops=js ;;
    jstests) tops="js xul-info.txt" ;;
    toolchain-clang) tops=clang ;;
    toolchain-sysroot) tops=sysroot-wasm32-wasi ;;
  esac
  # shellcheck disable=SC2086 # tops are fixed words
  seen=$(check_members "$dl/$base-$k.tar.zst" $tops) || die "$base-$k.tar.zst: refused"
  case $k in
    toolchain-* | wasm32-wasi) [ "$seen" = "$tops" ] || die "$base-$k.tar.zst: top level is '$seen', want $tops" ;;
  esac
done

# Everything verified: extract.
mkdir -p "$dest/toolchain" "$dest/testsrc"
untar() { zstd -q -dc "$1" | tar -xf - -C "$2" || die "${1##*/}: extraction failed"; }
untar "$dl/$base-wasm32-wasi.tar.zst" "$dest"
untar "$dl/$base-toolchain-clang.tar.zst" "$dest/toolchain"
untar "$dl/$base-toolchain-sysroot.tar.zst" "$dest/toolchain"
untar "$dl/$base-testsrc.tar.zst" "$dest/testsrc"
if [ "$jstests" = true ]; then
  untar "$dl/$base-jstests.tar.zst" "$dest/testsrc"
fi

if [ ! -x "$dest/toolchain/clang/bin/clang" ] || [ ! -x "$dest/toolchain/clang/bin/clang++" ]; then
  die "$base-toolchain-clang.tar.zst: clang/bin/clang and clang/bin/clang++ must be executables"
fi
[ -d "$dest/toolchain/sysroot-wasm32-wasi/include" ] || die "$base-toolchain-sysroot.tar.zst: no include/"
[ -f "$dest/spidermonkey/include-private/js-build-config.json" ] || die "$base-wasm32-wasi.tar.zst: no include-private/js-build-config.json"
[ -f "$dest/testsrc/js/src/jit-test/jit_test.py" ] || die "$base-testsrc.tar.zst: no js/src/jit-test/jit_test.py"
rm -rf "$dl"

out="tag=$tag
spidermonkey_dist=$dest/spidermonkey
toolchain_root=$dest/toolchain/clang
sysroot=$dest/toolchain/sysroot-wasm32-wasi
testsrc=$dest/testsrc"
printf '%s\n' "$out"
if [ -n "${GITHUB_OUTPUT:-}" ]; then
  printf '%s\n' "$out" >>"$GITHUB_OUTPUT"
fi
