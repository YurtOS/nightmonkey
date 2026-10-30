"""Test the nightly gate: a scheduled run of an unchanged yurt repeats the
last scheduled result in seconds instead of rebuilding."""

import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[1] / "nightly-gate.py"
URL = "https://github.com/YurtOS/nightmonkey/actions/runs/7"


def run(head_sha, conclusion):
    return {"head_sha": head_sha, "conclusion": conclusion, "html_url": URL}


class NightlyGateTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="night gate ")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.calls = self.root / "calls"
        gh = self.bin / "gh"
        # A fake gh: records its argv and answers with FAKE_GH_OUTPUT/STATUS.
        gh.write_text(
            "#!/usr/bin/env python3\n"
            "import os, sys\n"
            "open(os.environ['FAKE_GH_CALLS'], 'a').write(' '.join(sys.argv[1:]) + '\\n')\n"
            "sys.stdout.write(os.environ.get('FAKE_GH_OUTPUT', ''))\n"
            "sys.exit(int(os.environ.get('FAKE_GH_STATUS', '0')))\n"
        )
        gh.chmod(0o755)
        self.output = self.root / "github-output"
        self.output.write_text("")

    def gate(self, event, head="abc", runs=None, status=0, raw=None, code=0):
        body = raw if raw is not None else json.dumps({"workflow_runs": runs or []})
        env = {**os.environ, "PATH": f"{self.bin}{os.pathsep}{os.environ['PATH']}",
               "FAKE_GH_CALLS": str(self.calls), "FAKE_GH_OUTPUT": body,
               "FAKE_GH_STATUS": str(status), "GITHUB_OUTPUT": str(self.output)}
        result = subprocess.run(
            ["python3", str(SCRIPT), "--event", event, "--head", head,
             "--repo", "YurtOS/nightmonkey", "--workflow", "jstests.yml", "--branch", "yurt"],
            env=env, capture_output=True, text=True, check=False)
        self.assertEqual(result.returncode, code, result.stdout + result.stderr)
        return self.output.read_text(), result.stdout + result.stderr

    def test_dispatch_always_runs_without_querying(self):
        out, _ = self.gate("workflow_dispatch", runs=[run("abc", "success")])
        self.assertEqual(out, "run=true\n")
        self.assertFalse(self.calls.exists())

    def test_unchanged_after_a_green_run_skips(self):
        out, log = self.gate("schedule", runs=[run("abc", "success")])
        self.assertEqual(out, "run=false\n")
        self.assertIn(URL, log)
        call = self.calls.read_text()
        self.assertIn("repos/YurtOS/nightmonkey/actions/workflows/jstests.yml/runs", call)
        for field in ("event=schedule", "status=completed", "branch=yurt"):
            self.assertIn(field, call)

    def test_unchanged_after_a_red_run_fails_fast_naming_it(self):
        out, log = self.gate("schedule", runs=[run("abc", "failure")], code=1)
        self.assertNotIn("run=true", out)
        self.assertIn("::error::", log)
        self.assertIn(URL, log)
        self.assertIn("dispatch", log)

    def test_unchanged_after_any_other_conclusion_runs(self):
        for conclusion in ("cancelled", "timed_out", "skipped", None):
            with self.subTest(conclusion=conclusion):
                out, _ = self.gate("schedule", runs=[run("abc", conclusion)])
                self.assertEqual(out, "run=true\n")
                self.output.write_text("")

    def test_schedule_runs_when_head_moved(self):
        for conclusion in ("success", "failure"):
            with self.subTest(conclusion=conclusion):
                out, _ = self.gate("schedule", runs=[run("old", conclusion)])
                self.assertEqual(out, "run=true\n")
                self.output.write_text("")

    def test_schedule_runs_when_there_is_no_previous_run(self):
        out, _ = self.gate("schedule", runs=[])
        self.assertEqual(out, "run=true\n")

    def test_api_failure_runs_and_warns(self):
        for status, raw in ((1, ""), (0, "not json"), (0, "{}")):
            with self.subTest(status=status, raw=raw):
                out, log = self.gate("schedule", status=status, raw=raw)
                self.assertEqual(out, "run=true\n")
                self.assertIn("::warning::", log)
                self.output.write_text("")


if __name__ == "__main__":
    unittest.main()
