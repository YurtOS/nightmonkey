"""Test the real wrappers at the upstream Python process boundary."""

import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path


class UpstreamHarnessTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="night tests ")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.scripts = self.root / "scripts"
        self.scripts.mkdir()
        tests = self.root / "tests"
        tests.mkdir()
        for suite in ("jit-test", "jstests"):
            for prefix in ("", "wasi-"):
                (tests / f"{prefix}{suite}-excludes.txt").write_text("")
        shutil.copyfile(Path(__file__).resolve().parents[1] / "check-test-log.py",
                        self.scripts / "check-test-log.py")
        shutil.copyfile(Path(__file__).resolve().parents[1] / "run-jstests-checked.py", self.scripts / "run-jstests-checked.py")
        self.build = self.root / "build"
        (self.build / "bin").mkdir(parents=True)
        wrapper = self.build / "bin/inproc-shell.sh"
        wrapper.write_text("#!/bin/sh\nexit 0\n")
        wrapper.chmod(0o755)
        self.firefox = self.root / "firefox"
        for script, upstream in (("run-jit-tests.sh", "jit-test/jit_test.py"),
                                 ("run-jstests.sh", "tests/jstests.py")):
            shutil.copyfile(Path(__file__).resolve().parents[1] / script, self.scripts / script)
            harness = self.firefox / "js/src" / upstream
            harness.parent.mkdir(parents=True)
            harness.write_text(
                'import os, sys\nprint(os.environ["HARNESS_OUTPUT"])\n'
                'sys.exit(int(os.environ.get("HARNESS_STATUS", "0")))\n'
            )

        lib = self.firefox / "js/src/tests/lib"
        lib.mkdir()
        (lib / "__init__.py").write_text("")
        (lib / "results.py").write_text("class ResultsSink:\n    def finish(self, completed): pass\n")

    def run_wrapper(self, suite, output, status=0):
        return subprocess.run(
            ["bash", str(self.scripts / ("run-jit-tests.sh" if suite == "jit" else "run-jstests.sh")), str(self.firefox), str(self.build)],
            env={**os.environ, "HARNESS_OUTPUT": output, "HARNESS_STATUS": str(status)},
            capture_output=True, text=True, check=False,
        )

    def test_jit_accepts_its_own_positive_summary(self):
        result = self.run_wrapper("jit", "TEST-PASS | first.js\nTEST-PASS | second.js\nPASSED ALL\nPassed: 2\nFailed: 0")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_unexpected_results_override_a_false_green_summary(self):
        for suite in ("jit", "js"):
            with self.subTest(suite=suite):
                result = self.run_wrapper(suite, "TEST-UNEXPECTED-FAIL | broken.js\n"
                                          "TEST-PASS | good.js\nPASSED ALL\nPassed: 2\nFailed: 0")
                self.assertEqual(result.returncode, 2, result.stdout + result.stderr)

    def test_jit_rejects_incomplete_or_inconsistent_totals(self):
        for output in ("TEST-PASS | good.js\nPASSED ALL\nPassed: 2\nFailed: 0",
                       "TEST-PASS | good.js\nPASSED ALL (partial run -- interrupted by user)\nPassed: 1\nFailed: 0"):
            with self.subTest(output=output):
                self.assertEqual(self.run_wrapper("jit", output).returncode, 2)

    def test_skip_only_jit_run_is_rejected(self):
        result = self.run_wrapper("jit", "TEST-PASS | skipped.js | Success (code 59, args \"\")\nPASSED ALL\nPassed: 1\nFailed: 0")
        self.assertEqual(result.returncode, 2)

    def test_interrupted_jstests_is_rejected(self):
        result = self.run_wrapper("js", "TEST-PASS | example.js\nNIGHT-JSTESTS completed=0 passed=1 failed=0 timeout=0 skipped=0")
        self.assertEqual(result.returncode, 2)

    def test_jstests_accepts_its_own_passing_result(self):
        result = self.run_wrapper("js", "TEST-PASS | example.js | (args: '') [0.1 s]\nNIGHT-JSTESTS completed=1 passed=1 failed=0 timeout=0 skipped=0")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_jit_rejects_jstests_output(self):
        self.assertEqual(self.run_wrapper("jit", "TEST-PASS | example.js").returncode, 2)

    def test_jstests_rejects_jit_output(self):
        self.assertEqual(self.run_wrapper("js", "Passed: 2\nFailed: 0").returncode, 2)

    def test_empty_and_zero_runs_are_rejected(self):
        for suite in ("jit", "js"):
            for output in ("", "No tests found matching command line arguments.", "Passed: 0\nFailed: 0"):
                with self.subTest(suite=suite, output=output):
                    self.assertEqual(self.run_wrapper(suite, output).returncode, 2)

    def test_upstream_failure_is_preserved_even_with_positive_output(self):
        for suite, output in (("jit", "Passed: 2"), ("js", "TEST-PASS | example.js")):
            with self.subTest(suite=suite):
                self.assertEqual(self.run_wrapper(suite, output, 7).returncode, 7)


if __name__ == "__main__":
    unittest.main()
