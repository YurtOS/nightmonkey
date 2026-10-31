#!/usr/bin/env bash
# Check that the engine-release job builds against the release pinned in
# engine/ENGINE_RELEASE, verifying every digest first, and nothing else.
# Comment lines in engine-release.yml are ignored.
#
# The steps that carry this (fetch the release: download, verify every digest,
# extract; build NightMonkey against what was fetched; run the jit-tests from
# the release's test sources; package them for the jit-test and jstests
# shards; run the jstests with full_jstests) must each appear once, in that
# order, with exactly the text in REQUIRED below: no other `if:`, no
# `continue-on-error:`, no other argument; changing one of them means changing
# this script too. In the engine job, no other step may mention the release
# directories or download anything (see below; this over-flags on purpose).
# No other step may mention the release pin, a download, the
# fetch script, cmake, or the source build (engine-src, mach, build-engine.sh,
# build-spidermonkey.sh, QUEUE_PIN), not even in echo text. Every step that
# runs jit-tests or jstests takes them from the release
# (NIGHT_FIREFOX_TESTS). The job may not carry `if:` or `continue-on-error:`,
# every action is pinned to a full commit SHA, nothing references the
# YurtOS/firefox fork, and engine/ENGINE_RELEASE holds a fixed tag
# engine-v<version>-<n> (never a floating one, `latest` or the PENDING
# placeholder) and a full sha256 for MANIFEST.json and each asset.
#
# This blocks accidental regressions, not deliberate sabotage by someone who
# can edit this repository, guard included. The source build it replaced
# (engine.yml, engine/QUEUE_PIN) is gone; the ban on its words keeps it out.
#
#   scripts/check-engine-release.sh
set -euo pipefail

here=$(cd "$(dirname "$0")/.." && pwd)

python3 -I - "$here/.github/workflows/engine-release.yml" "$here/engine/ENGINE_RELEASE" <<'PY'
import re
import sys

workflow, release_file = sys.argv[1:]

REQUIRED = [
    ("the fetch (download, verify every digest against engine/ENGINE_RELEASE, extract)", r'''
- name: Fetch the engine release
  id: release
  env:
    GH_TOKEN: ${{ github.token }}
    FULL_JSTESTS: ${{ inputs.full_jstests || inputs.jstests_inputs }}
  run: scripts/fetch-engine-release.sh --jstests "$FULL_JSTESTS" "$RUNNER_TEMP"
'''),
    ("the build (against the fetched engine and toolchain)", r'''
- name: Build NightMonkey against the release
  id: build
  run: |
    cmake -S . -B build -DSPIDERMONKEY_DIST="$RUNNER_TEMP/spidermonkey" -DNIGHT_TOOLCHAIN_ROOT="$RUNNER_TEMP/toolchain/clang" -DNIGHT_SYSROOT="$RUNNER_TEMP/toolchain/sysroot-wasm32-wasi"
    cmake --build build
    test -x build/bin/inproc-shell.sh
'''),
    ("the jit-tests (from the fetched test sources)", r'''
- name: jit-tests from the release test sources
  if: ${{ !cancelled() && steps.build.outcome == 'success' && inputs.jit_tests != 'sharded' && inputs.jit_tests != 'none' }}
  timeout-minutes: 125
  env:
    JIT_TESTS: ${{ inputs.jit_tests }}
    NIGHT_FIREFOX_TESTS: ${{ runner.temp }}/testsrc
  run: |
    set -euo pipefail
    case "$JIT_TESTS" in
      full) scripts/ci-suite.sh jit "$CI_LANE" ;;
      subset)
        read -ra paths <<< "$JIT_SUBSET"
        scripts/ci-suite.sh jit "$CI_LANE" -- "${paths[@]}" ;;
      *) echo "::error::unknown jit_tests input: $JIT_TESTS"; exit 1 ;;
    esac
'''),
    ("the shard inputs (the built shell and the fetched test sources)", r'''
- name: Package the shard inputs
  if: ${{ !cancelled() && steps.build.outcome == 'success' && inputs.jit_tests == 'sharded' }}
  run: |
    set -euo pipefail
    tar -C "$GITHUB_WORKSPACE" -cf "$RUNNER_TEMP/shard-inputs.tar" build/bin
    tar -C "$RUNNER_TEMP/testsrc" -rf "$RUNNER_TEMP/shard-inputs.tar" js
    zstd -q -T0 --rm "$RUNNER_TEMP/shard-inputs.tar"
    ls -la "$RUNNER_TEMP/shard-inputs.tar.zst"
'''),
    ("the jstests inputs (the built shell, the fetched test sources and xul-info.txt)", r'''
- name: Package the jstests inputs
  if: ${{ !cancelled() && steps.build.outcome == 'success' && inputs.jstests_inputs }}
  run: |
    set -euo pipefail
    tar -C "$GITHUB_WORKSPACE" -cf "$RUNNER_TEMP/jstests-inputs.tar" build/bin
    tar -C "$RUNNER_TEMP/testsrc" -rf "$RUNNER_TEMP/jstests-inputs.tar" js xul-info.txt
    zstd -q -T0 --rm "$RUNNER_TEMP/jstests-inputs.tar"
    ls -la "$RUNNER_TEMP/jstests-inputs.tar.zst"
'''),
    ("the jstests (from the fetched test sources, with the jstests bundle)", r'''
- name: Full jstests
  if: ${{ !cancelled() && steps.build.outcome == 'success' && inputs.full_jstests }}
  timeout-minutes: 245
  env:
    NIGHT_FIREFOX_TESTS: ${{ runner.temp }}/testsrc
  run: scripts/ci-suite.sh jstests "$CI_LANE"
'''),
]
TESTSRC = "NIGHT_FIREFOX_TESTS: ${{ runner.temp }}/testsrc"

errors = []


def err(msg):
    errors.append(msg)


def indent(line):
    return len(line) - len(line.lstrip(" "))


lines = [l.rstrip() for l in open(workflow).read().split("\n")]
lines = [l for l in lines if l.strip() and not l.lstrip().startswith("#")]

for l in lines:
    if re.search(r"FIREFOX_REF|FIREFOX_REPO|YurtOS/firefox(?![A-Za-z0-9_-])", l):
        err("engine-release.yml references the YurtOS/firefox fork: " + l.strip())
    if re.match(r"^\s*continue-on-error:", l):
        err("engine-release.yml uses continue-on-error: " + l.strip())
    m = re.match(r"^    (if|continue-on-error):", l)
    if m:
        err("engine-release.yml has a job-level %s: %s" % (m.group(1), l.strip()))

steps = []
step_jobs = []  # the job (`  <name>:` under jobs:) each step belongs to


def job_at(k):
    for j in range(k, -1, -1):
        m = re.match(r"^  ([\w-]+):\s*$", lines[j])
        if m:
            return m.group(1)
    return None


i = 0
while i < len(lines):
    if lines[i].strip() != "steps:":
        i += 1
        continue
    job = job_at(i)
    i += 1
    if i >= len(lines) or not lines[i].lstrip().startswith("- "):
        continue
    item = indent(lines[i])
    while i < len(lines) and indent(lines[i]) == item and lines[i].lstrip().startswith("- "):
        body = [lines[i][item:]]
        i += 1
        while i < len(lines) and indent(lines[i]) > item:
            body.append(lines[i][item:])
            i += 1
        steps.append("\n".join(body))
        step_jobs.append(job)
if not steps:
    err("engine-release.yml has no steps")


def name_of(step):
    m = re.search(r"^(?:- |  )name: (.*)$", step, re.M)
    return m.group(1) if m else step.split("\n")[0]


def body_lines(step):
    return [l for l in step.split("\n") if not re.match(r"^(?:- |  )name: ", l)]


for step in steps:
    for raw in re.findall(r"^(?:- |  )uses: (.*)$", step, re.M):
        if not re.fullmatch(r"[\w.-]+/[\w./-]+@[0-9a-f]{40}\s+#\s*v\d+(\.\d+)*", raw.strip()):
            err("action not pinned to a full commit SHA with a # vX.Y.Z comment: " + raw.strip())

found = []
for label, text in REQUIRED:
    want = text.strip("\n")
    name = name_of(want)
    at = [k for k, s in enumerate(steps) if name_of(s) == name]
    if len(at) != 1:
        err("%s: need exactly one step named '%s', found %d; it must be exactly:\n%s" % (label, name, len(at), want))
        continue
    if steps[at[0]] != want:
        err("%s: step '%s' must be exactly:\n%s\nbut is:\n%s" % (label, name, want, steps[at[0]]))
    found.append(at[0])
if len(found) == len(REQUIRED) and found != sorted(found):
    err("step order must be: fetch, build, jit-tests, shard inputs, jstests inputs, jstests")

required_names = {name_of(text.strip("\n")) for _, text in REQUIRED}
for step in steps:
    if name_of(step) in required_names:
        continue
    hits = [l.strip() for l in body_lines(step)
            if re.search(r"ENGINE_RELEASE|fetch-engine-release|gh release|\bcmake\b|engine-src|\bmach\b|"
                         r"build-engine\.sh|build-spidermonkey\.sh|QUEUE_PIN", l)]
    if hits:
        err("step '%s' mentions the release pin, a download, cmake or the source build (%s); only the "
            "required steps may, even in echo text" % (name_of(step), hits[0]))
    if re.search(r"ci-suite\.sh (jit|jstests)\b", step) and TESTSRC not in body_lines(step) and \
            ("    " + TESTSRC) not in step.split("\n"):
        err("step '%s' runs jit-tests or jstests without %s" % (name_of(step), TESTSRC))

# Engine bytes come only from the fetch step. In the job that runs it (other
# jobs are not checked), only the steps pinned above may MENTION the release
# directories at all: any line of another step that names spidermonkey,
# toolchain or testsrc as a word, however the path around it is spelled,
# fails, a read or a comment included, and so does curl, wget, git clone or
# fetch, gh release or api, download-artifact or the word repository anywhere
# (a checkout of another repository, in a block or flow mapping). This over-flags on purpose: no shell is
# parsed, so a new step that needs the release directories is added to
# REQUIRED above, deliberately. Only full-line comments are ignored.
DOWNLOADS = re.compile(r"\bcurl\b|\bwget\b|\bgit\b.*\b(?:clone|fetch)\b|\bgh\s+(?:release|api)\b|"
                       r"download-artifact|\brepository\b", re.I)
RELEASE_WORD = re.compile(r"\b(?:spidermonkey|toolchain|testsrc)\b", re.I)


def normalized(line):
    # Quote characters dropped, so a quoted `"repository":` key matches too.
    return line.replace('"', "").replace("'", "")


engine_jobs = {step_jobs[k] for k, s in enumerate(steps) if name_of(s) == "Fetch the engine release"}
for k, step in enumerate(steps):
    if step_jobs[k] not in engine_jobs or name_of(step) in required_names:
        continue
    for l in body_lines(step):
        t = normalized(l)
        if DOWNLOADS.search(t):
            err("step '%s' downloads, clones or checks out something (%s); in the engine job only the "
                "fetch step brings anything in" % (name_of(step), l.strip()))
        if RELEASE_WORD.search(t):
            err("step '%s' mentions the release directories (%s); only the pinned steps may, even to "
                "read them: add a step that needs them to REQUIRED in this guard" % (name_of(step), l.strip()))

KEYS = ["tag", "manifest", "wasm32-wasi", "testsrc", "jstests", "toolchain-clang", "toolchain-sysroot"]
try:
    text = open(release_file).read()
except FileNotFoundError:
    err("engine/ENGINE_RELEASE is missing")
else:
    got = []
    for line in text.split("\n")[:-1] if text.endswith("\n") else text.split("\n"):
        if not line or line.startswith("#"):
            continue
        k, sep, v = line.partition("=")
        if not sep:
            err("engine/ENGINE_RELEASE: not key=value: %r" % line)
            continue
        got.append((k, v))
    if not text.endswith("\n"):
        err("engine/ENGINE_RELEASE must end with a newline")
    names = [k for k, _ in got]
    for k in KEYS:
        if names.count(k) == 0:
            err("engine/ENGINE_RELEASE: %s is missing" % k)
        elif names.count(k) > 1:
            err("engine/ENGINE_RELEASE: %s appears %d times" % (k, names.count(k)))
    for k in names:
        if k not in KEYS:
            err("engine/ENGINE_RELEASE: unknown key %s" % k)
    if [k for k in names if k in KEYS] != [k for k in KEYS if k in names]:
        err("engine/ENGINE_RELEASE: keys must be in the order %s" % ", ".join(KEYS))
    d = dict(got)
    if "tag" in d and not re.fullmatch(r"engine-v[0-9]+(\.[0-9]+)+-[1-9][0-9]*", d["tag"]):
        err("engine/ENGINE_RELEASE: tag %r is not a fixed engine-v<version>-<n> (a floating tag, latest or "
            "the PENDING placeholder is refused; run scripts/update-engine-release.sh <tag>)" % d["tag"])
    for k in KEYS[1:]:
        if k in d and not re.fullmatch(r"[0-9a-f]{64}", d[k]):
            err("engine/ENGINE_RELEASE: %s=%r is not a sha256 (PENDING until the release is pinned)" % (k, d[k]))

for e in errors:
    print("check-engine-release: " + e, file=sys.stderr)
if errors:
    sys.exit(1)
print("check-engine-release: ok")
PY
