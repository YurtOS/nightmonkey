#!/usr/bin/env bash
# Check that CI builds the engine from the YurtOS/yurt-firefox patch queue at the
# commit in engine/QUEUE_PIN, and only after verifying that commit, not from a
# branch of the frozen fork. It reads the structure of engine.yml's steps
# (comment lines ignored), not substrings:
#   - every action is pinned to a full commit SHA (`# vX.Y.Z` comment);
#   - a step with `id: pin` reads engine/QUEUE_PIN and outputs it as sha;
#   - the one YurtOS/yurt-firefox checkout uses `ref: ${{ steps.pin.outputs.sha }}`
#     into engine-src, after the pin step;
#   - after it and before the build, a step fetches yurt-firefox main and fails
#     unless the pin (env PIN from steps.pin) is an ancestor of origin/main, and
#     a step fails unless spidermonkey/mozconfig equals engine-src/mozconfig;
#   - exactly one step runs engine-src/scripts/build-engine.sh (none runs
#     build-spidermonkey.sh), and it copies build-manifest.json into the logs;
#   - nothing references the YurtOS/firefox fork;
# and that engine/QUEUE_PIN is exactly one line of 40 lowercase hex digits.
#
#   scripts/check-engine-source.sh
set -euo pipefail

here=$(cd "$(dirname "$0")/.." && pwd)

python3 -I - "$here/.github/workflows/engine.yml" "$here/engine/QUEUE_PIN" <<'PY'
import re
import sys

workflow, pin_file = sys.argv[1:]
errors = []


def err(msg):
    errors.append(msg)


def indent(line):
    return len(line) - len(line.lstrip(" "))


lines = [l for l in open(workflow).read().split("\n") if not l.lstrip().startswith("#")]

for l in lines:
    if re.search(r"FIREFOX_REF|FIREFOX_REPO|YurtOS/firefox(?![A-Za-z0-9_-])", l):
        err("engine.yml references the YurtOS/firefox fork: " + l.strip())


def scalar(value):
    return re.sub(r"\s+#.*$", "", value).strip()


def parse_step(body, key_indent):
    """One step: scalar keys, `with`/`env` mappings, `run` lines, raw `uses`."""
    step = {"keys": {}, "with": {}, "env": {}, "run": [], "uses_raw": None}
    j = 0
    while j < len(body):
        m = re.match(r"^(\s*)([A-Za-z_-]+):\s?(.*)$", body[j])
        if not m or len(m.group(1)) != key_indent:
            j += 1
            continue
        key, value = m.group(2), m.group(3)
        j += 1
        child = []
        while j < len(body) and (not body[j].strip() or indent(body[j]) > key_indent):
            child.append(body[j])
            j += 1
        if key == "run":
            if value.strip() in ("|", ">"):
                block = [c for c in child if c.strip()]
                base = min((indent(c) for c in block), default=0)
                step["run"] = [c[base:] for c in block if not c.strip().startswith("#")]
            else:
                step["run"] = [value.strip()]
        elif key in ("with", "env"):
            for c in child:
                cm = re.match(r"^\s*([A-Za-z_-]+):\s?(.*)$", c)
                if cm:
                    step[key][cm.group(1)] = scalar(cm.group(2))
        else:
            step["keys"][key] = scalar(value)
            if key == "uses":
                step["uses_raw"] = value.strip()
    return step


def parse_steps(lines):
    steps = []
    i = 0
    while i < len(lines):
        if lines[i].strip() != "steps:":
            i += 1
            continue
        i += 1
        while i < len(lines) and not lines[i].strip():
            i += 1
        if i >= len(lines) or not lines[i].lstrip().startswith("- "):
            continue
        item = indent(lines[i])
        while i < len(lines):
            line = lines[i]
            if not line.strip():
                i += 1
                continue
            if indent(line) < item or not line.lstrip().startswith("- ") or indent(line) != item:
                break
            body = [" " * (item + 2) + line.lstrip()[2:]]
            i += 1
            while i < len(lines) and (not lines[i].strip() or indent(lines[i]) > item):
                body.append(lines[i])
                i += 1
            steps.append(parse_step(body, item + 2))
    return steps


steps = parse_steps(lines)
if not steps:
    err("engine.yml has no steps")

for s in steps:
    raw = s["uses_raw"]
    if raw is not None and not re.fullmatch(r"[\w.-]+/[\w./-]+@[0-9a-f]{40}\s+#\s*v\d+(\.\d+)*", raw):
        err("action not pinned to a full commit SHA with a # vX.Y.Z comment: " + raw)


def find(pred):
    return [i for i, s in enumerate(steps) if pred(s)]


def runs(s, pattern):
    return any(re.search(pattern, l) for l in s["run"])


PIN_EXPR = "${{ steps.pin.outputs.sha }}"

pin = find(lambda s: s["keys"].get("id") == "pin")
if len(pin) != 1:
    err("engine.yml needs exactly one step with id: pin")
elif not (runs(steps[pin[0]], r'^pin=\$\((cat |<\s*)engine/QUEUE_PIN\)$')
          and runs(steps[pin[0]], r'^echo "sha=\$pin" >> "\$GITHUB_OUTPUT"$')):
    err("the id: pin step does not read engine/QUEUE_PIN into pin and output sha=$pin")

checkout = find(lambda s: s["with"].get("repository") == "YurtOS/yurt-firefox")
if len(checkout) != 1:
    err("engine.yml needs exactly one checkout of YurtOS/yurt-firefox")
else:
    w = steps[checkout[0]]["with"]
    if w.get("ref") != PIN_EXPR:
        err("the YurtOS/yurt-firefox checkout must use ref: %s, not %r" % (PIN_EXPR, w.get("ref")))
    if w.get("path") != "engine-src":
        err("the YurtOS/yurt-firefox checkout must use path: engine-src")

fetch = find(lambda s: runs(s, r"^git -C engine-src fetch\b.*\brefs/heads/main:refs/remotes/origin/main\b"))
if not fetch:
    err("no step fetches yurt-firefox main into engine-src "
        "(git -C engine-src fetch ... refs/heads/main:refs/remotes/origin/main)")
ancestor = find(lambda s: s["env"].get("PIN") == PIN_EXPR and runs(
    s, r'^git -C engine-src merge-base --is-ancestor "\$PIN" origin/main \|\| \{.*\bexit 1; \}$'))
if not ancestor:
    err('no step fails unless the pin is an ancestor of origin/main (env PIN: %s; '
        'git -C engine-src merge-base --is-ancestor "$PIN" origin/main || { ...; exit 1; })' % PIN_EXPR)
mozconfig = find(lambda s: runs(s, r"^cmp spidermonkey/mozconfig engine-src/mozconfig \|\| \{.*\bexit 1; \}$"))
if not mozconfig:
    err("no step fails unless spidermonkey/mozconfig equals engine-src/mozconfig")

build = find(lambda s: runs(s, r"^engine-src/scripts/build-engine\.sh\b"))
if len(build) != 1:
    err("engine.yml must run engine-src/scripts/build-engine.sh in exactly one step")
for s in steps:
    if runs(s, r"build-spidermonkey\.sh"):
        err("engine.yml runs scripts/build-spidermonkey.sh; CI builds with build-engine.sh")
manifest = find(lambda s: runs(s, r'^cp "\$GITHUB_WORKSPACE/build-manifest\.json" "\$RUNNER_TEMP/nightmonkey-logs/"$'))
if not manifest:
    err("no step copies build-manifest.json into $RUNNER_TEMP/nightmonkey-logs/")

if len(pin) == 1 and len(checkout) == 1 and fetch and ancestor and mozconfig and len(build) == 1 and manifest:
    p, c, b = pin[0], checkout[0], build[0]
    checks = (min(fetch), min(ancestor), min(mozconfig))
    if not (p < c < min(checks) and max(checks) < b <= min(manifest)):
        err("step order must be: pin, checkout, fetch/ancestry/mozconfig checks, build, manifest copy")

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
