"""The AOT jstests run weekly in five shards; the nightly runs the interpreter lane.

jstests.py --total-chunks slices the test list in the order os.walk yields
it, which is not sorted: shards on different runners could overlap or miss
tests. run-jstests-checked.py sorts the walk so every shard slices the same
list."""

import os
import re
import subprocess
import tempfile
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
WORKFLOWS = REPO / ".github/workflows"


def body(name):
    """The workflow text without full-line comments."""
    return "\n".join(l for l in (WORKFLOWS / name).read_text().splitlines()
                     if not l.lstrip().startswith("#")) + "\n"


def job(text, name):
    m = re.search(rf"^  {name}:\n((?:    .*\n|\n)*)", text, re.M)
    return m.group(1) if m else ""


def jobs(text):
    section = text[text.index("\njobs:\n"):]
    return re.findall(r"^  ([\w-]+):$", section, re.M)


def step(text, name):
    lines = text.splitlines()
    start = next(i for i, l in enumerate(lines) if l.strip() == f"- name: {name}")
    indent = len(lines[start]) - len(lines[start].lstrip())
    out = [lines[start]]
    for l in lines[start + 1:]:
        if l.strip() and len(l) - len(l.lstrip()) <= indent:
            break
        out.append(l)
    return "\n".join(out)


def value(text, key):
    m = re.search(rf"^\s+{key}: (.*)$", text, re.M)
    return m.group(1).strip() if m else None


class SortedWalkTests(unittest.TestCase):
    def test_the_wrapper_sorts_the_test_walk(self):
        with tempfile.TemporaryDirectory() as d:
            tests = Path(d, "js/src/tests")
            lib = tests / "lib"
            lib.mkdir(parents=True)
            (lib / "__init__.py").write_text("")
            (lib / "results.py").write_text("class ResultsSink:\n    def finish(self, completed): pass\n")
            (lib / "manifest.py").write_text(
                "def _find_all_js_files(location):\n"
                "    yield from [('test262/b', 'z.js'), ('non262', 'b.js'), ('test262/a', 'y.js'), ('non262', 'a.js')]\n"
                "def count_tests(location):\n"
                "    return list(_find_all_js_files(location))\n")
            (tests / "jstests.py").write_text(
                "import lib.manifest as manifest\nprint(manifest.count_tests('.'))\n")
            out = subprocess.run(["python3", str(REPO / "scripts/run-jstests-checked.py"),
                                  str(tests / "jstests.py")],
                                 capture_output=True, text=True, check=True).stdout
        self.assertEqual(out.strip(), str([("non262", "a.js"), ("non262", "b.js"),
                                           ("test262/a", "y.js"), ("test262/b", "z.js")]))


class NightlyTests(unittest.TestCase):
    def test_the_nightly_runs_the_interpreter_lane_only(self):
        text = body("jstests.yml")
        self.assertEqual(jobs(text), ["gate", "interpreter"])
        self.assertNotIn("lane: aot", text)
        self.assertIn("lane: interpreter", job(text, "interpreter"))
        self.assertIn("uses: ./.github/workflows/engine-release.yml", job(text, "interpreter"))
        self.assertIn("full_jstests: true", job(text, "interpreter"))


class WeeklyAotTests(unittest.TestCase):
    def setUp(self):
        self.text = body("jstests-aot.yml")
        self.shard = job(self.text, "shard")

    def test_weekly_schedule_and_dispatch(self):
        cron = re.search(r"- cron: '([^']+)'", self.text).group(1).split()
        self.assertEqual(len(cron), 5)
        self.assertNotIn(cron[4], ("*", "?"), "the day-of-week field makes it weekly")
        self.assertEqual(cron[2], "*")
        self.assertIn("  workflow_dispatch:", self.text)

    def test_the_gate_reads_this_workflows_own_runs(self):
        gate = job(self.text, "gate")
        self.assertIn("scripts/nightly-gate.py", gate)
        self.assertIn("--workflow jstests-aot.yml", gate)
        engine = job(self.text, "engine")
        self.assertIn("needs: gate", engine)
        self.assertIn("if: ${{ needs.gate.outputs.run == 'true' }}", engine)

    def test_one_aot_build_without_jit_tests_that_packages_the_jstests(self):
        engine = job(self.text, "engine")
        self.assertIn("uses: ./.github/workflows/engine-release.yml", engine)
        self.assertIn("lane: aot", engine)
        self.assertIn("jit_tests: none", engine)
        self.assertIn("jstests_inputs: true", engine)
        self.assertNotIn("full_jstests", engine)

    def test_shards_cover_one_through_shards(self):
        shards = int(re.search(r"^  SHARDS: (\d+)$", self.text, re.M).group(1))
        # Five: the per-test times of run 41086602517 project 41-51 min per
        # shard (four would put about 66 min in the first).
        self.assertEqual(shards, 5)
        matrix = re.search(r"shard: \[([\d, ]+)\]", self.shard).group(1)
        self.assertEqual([int(s) for s in matrix.split(",")], list(range(1, shards + 1)))
        self.assertIn("needs: engine", self.shard)
        run = step(self.shard, "AOT jstests (shard ${{ matrix.shard }})")
        self.assertIn('scripts/ci-suite.sh jstests aot -- --total-chunks="$SHARDS" --this-chunk="$SHARD"', run)

    def test_the_shard_downloads_what_the_engine_uploads(self):
        upload = step(body("engine-release.yml"), "Upload the jstests inputs")
        download = step(self.shard, "Download the jstests inputs")
        self.assertEqual(value(upload, "name"), "nightmonkey-jstests-inputs-${{ inputs.lane }}")
        self.assertEqual(value(download, "name"), "nightmonkey-jstests-inputs-aot")
        self.assertEqual(value(upload, "overwrite"), "true")
        # "Re-run failed jobs" on a weekly run can come days later.
        self.assertGreaterEqual(int(value(upload, "retention-days")), 7)
        for text in (upload, download):
            self.assertNotIn("run_attempt", value(text, "name"))

    def test_the_jstests_inputs_hold_the_shell_and_the_whole_test_tree(self):
        package = step(body("engine-release.yml"), "Package the jstests inputs")
        self.assertIn("if: ${{ !cancelled() && steps.build.outcome == 'success' && inputs.jstests_inputs }}",
                      package)
        self.assertIn("build/bin", package)
        # The release test sources with the jstests bundle merged in, and its
        # xul-info.txt, which run-jstests.sh reads from the test root.
        self.assertIn('tar -C "$RUNNER_TEMP/testsrc" -rf "$RUNNER_TEMP/jstests-inputs.tar" js xul-info.txt', package)

    def test_engine_can_skip_the_jit_tests(self):
        jit = step(body("engine-release.yml"), "jit-tests from the release test sources")
        self.assertIn("inputs.jit_tests != 'none'", jit)

    def test_the_shard_limits_nest(self):
        # ci-suite's jstests limit must fit the step, and the step the job.
        seconds = int(re.search(r"jstests\) seconds=(\d+);", (REPO / "scripts/ci-suite.sh").read_text()).group(1))
        run = step(self.shard, "AOT jstests (shard ${{ matrix.shard }})")
        step_minutes = int(value(run, "timeout-minutes"))
        job_minutes = int(re.search(r"^    timeout-minutes: (\d+)$", self.shard, re.M).group(1))
        # At most about 51 min of AOT tests per shard (run 41086602517).
        self.assertGreaterEqual(seconds, 80 * 60)
        self.assertGreater(step_minutes * 60, seconds + 30)
        self.assertGreater(job_minutes, step_minutes)


if __name__ == "__main__":
    unittest.main()
