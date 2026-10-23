#!/usr/bin/env bash
# Check that CI builds the engine from the YurtOS/yurt-firefox patch queue at the
# commit in engine/QUEUE_PIN, and only after verifying that commit, not from a
# branch of the frozen fork. Comment lines in engine.yml are ignored.
#
# The five steps that carry this (read the pin, check out the queue at it,
# verify it is on yurt-firefox main, compare the mozconfigs, build) must each
# appear once, in that order, with exactly the text in REQUIRED below: no added
# `if:`, `continue-on-error:`, refspec or command. Changing one of them means
# changing this script too. No other step may mention engine-src, mach,
# build-engine.sh or build-spidermonkey.sh in any of its keys, whatever their
# order, not even in echo text (the rule is deliberately conservative). The job
# may not carry `if:` or `continue-on-error:`, every action is pinned to a full
# commit SHA, nothing references the YurtOS/firefox fork, and engine/QUEUE_PIN is
# exactly one line of 40 lowercase hex digits.
#
# This blocks accidental regressions (a check switched off, an unpinned build
# slipped in), not deliberate sabotage by someone who can edit this repository,
# guard included: an obfuscated name (variable indirection, YAML escapes,
# GITHUB_ENV) is out of its reach.
#
#   scripts/check-engine-source.sh
set -euo pipefail

here=$(cd "$(dirname "$0")/.." && pwd)

python3 -I - "$here/.github/workflows/engine.yml" "$here/engine/QUEUE_PIN" <<'PY'
import re
import sys

workflow, pin_file = sys.argv[1:]

# The critical steps, in order, as they must appear (indentation relative to
# the step's `- `; blank and comment lines ignored).
REQUIRED = [
    ("the pin step (id: pin, reads engine/QUEUE_PIN)", r'''
- name: Read the engine queue pin
  id: pin
  run: |
    pin=$(cat engine/QUEUE_PIN)
    if ! [[ "$pin" =~ ^[0-9a-f]{40}$ ]]; then
      echo "::error::engine/QUEUE_PIN is not a 40-hex commit SHA"
      exit 1
    fi
    echo "sha=$pin" >> "$GITHUB_OUTPUT"
    echo "Engine queue: YurtOS/yurt-firefox@$pin" | tee "$RUNNER_TEMP/nightmonkey-logs/engine.txt"
    echo "Engine queue: YurtOS/yurt-firefox@$pin" >> "$GITHUB_STEP_SUMMARY"
'''),
    ("the queue checkout (ref: ${{ steps.pin.outputs.sha }})", r'''
- name: Check out the engine queue
  uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
  with:
    repository: YurtOS/yurt-firefox
    ref: ${{ steps.pin.outputs.sha }}
    path: engine-src
    fetch-depth: 0
    persist-credentials: false
'''),
    ("the ancestry check (fetch main; the pin must be an ancestor of origin/main)", r'''
- name: Verify the engine queue pin is on yurt-firefox main
  env:
    PIN: ${{ steps.pin.outputs.sha }}
  run: |
    git -C engine-src fetch --no-tags origin +refs/heads/main:refs/remotes/origin/main
    test "$(git -C engine-src rev-parse HEAD)" = "$PIN" || { echo "::error::engine-src is not at $PIN"; exit 1; }
    git -C engine-src merge-base --is-ancestor "$PIN" origin/main || { echo "::error::engine/QUEUE_PIN $PIN is not on YurtOS/yurt-firefox main"; exit 1; }
'''),
    ("the mozconfig check (spidermonkey/mozconfig equals engine-src/mozconfig)", r'''
- name: Check spidermonkey/mozconfig matches the engine queue
  run: |
    cmp spidermonkey/mozconfig engine-src/mozconfig || { echo "::error::spidermonkey/mozconfig differs from YurtOS/yurt-firefox's mozconfig at engine/QUEUE_PIN: change it there and bump engine/QUEUE_PIN"; exit 1; }
'''),
    ("the build (engine-src/scripts/build-engine.sh; copies build-manifest.json into the logs)", r'''
- name: Build the engine (patched SpiderMonkey)
  run: |
    engine-src/scripts/build-engine.sh --workdir "$GITHUB_WORKSPACE/firefox"
    cp "$GITHUB_WORKSPACE/build-manifest.json" "$RUNNER_TEMP/nightmonkey-logs/"
'''),
]

errors = []


def err(msg):
    errors.append(msg)


def indent(line):
    return len(line) - len(line.lstrip(" "))


lines = [l.rstrip() for l in open(workflow).read().split("\n")]
lines = [l for l in lines if l.strip() and not l.lstrip().startswith("#")]

for l in lines:
    if re.search(r"FIREFOX_REF|FIREFOX_REPO|YurtOS/firefox(?![A-Za-z0-9_-])", l):
        err("engine.yml references the YurtOS/firefox fork: " + l.strip())
    if re.match(r"^\s*continue-on-error:", l):
        err("engine.yml uses continue-on-error: " + l.strip())
    m = re.match(r"^    (if|continue-on-error):", l)
    if m:
        err("engine.yml has a job-level %s: %s" % (m.group(1), l.strip()))

# Steps: the text of each `- ` item under a `steps:` key, dedented to the item.
steps = []
i = 0
while i < len(lines):
    if lines[i].strip() != "steps:":
        i += 1
        continue
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
if not steps:
    err("engine.yml has no steps")


def name_of(step):
    """The step's name: key, wherever it is among the keys; else its first line."""
    m = re.search(r"^(?:- |  )name: (.*)$", step, re.M)
    return m.group(1) if m else step.split("\n")[0]


def body_lines(step):
    """Every line of the step except its key-level name: line, whatever the key
    order: run, env, with and any other key are all searched."""
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
    err("step order must be: pin, queue checkout, ancestry check, mozconfig check, build")

required_names = {name_of(text.strip("\n")) for _, text in REQUIRED}
for step in steps:
    if name_of(step) in required_names:
        continue
    hits = [l.strip() for l in body_lines(step)
            if re.search(r"engine-src|\bmach\b|build-engine\.sh|build-spidermonkey\.sh", l)]
    if hits:
        err("step '%s' mentions engine-src, mach, build-engine.sh or build-spidermonkey.sh (%s); "
            "only the required steps may, even in echo text: move the text out of non-critical "
            "steps or update this guard" % (name_of(step), hits[0]))

try:
    data = open(pin_file, "rb").read()
except FileNotFoundError:
    err("engine/QUEUE_PIN is missing")
else:
    if not re.fullmatch(rb"[0-9a-f]{40}\n", data):
        err("engine/QUEUE_PIN must be exactly one line holding a 40-hex lowercase commit SHA")

for e in errors:
    print("check-engine-source: " + e, file=sys.stderr)
if errors:
    sys.exit(1)
print("check-engine-source: ok")
PY
