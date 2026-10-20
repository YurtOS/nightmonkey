"""Exercise the regression harness with a temporary shell and test tree."""

import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path


class NightTestHarnessTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        (self.root / "scripts").mkdir()
        (self.root / "tests/jit-test").mkdir(parents=True)
        shutil.copyfile(
            Path(__file__).resolve().parents[1] / "run-night-tests.sh",
            self.root / "scripts/run-night-tests.sh",
        )

    def shell(self):
        wrapper = self.root / "build/bin/inproc-shell.sh"
        wrapper.parent.mkdir(parents=True)
        wrapper.write_text('#!/usr/bin/env bash\nexec bash "$@"\n')
        wrapper.chmod(0o755)

    def run_harness(self):
        return subprocess.run(
            ["bash", str(self.root / "scripts/run-night-tests.sh")],
            capture_output=True, text=True, check=False,
        )

    def test_empty_suite_is_a_setup_failure(self):
        self.shell()
        result = self.run_harness()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("no regression tests", result.stderr)

    def test_missing_wrapper_is_a_setup_failure(self):
        result = self.run_harness()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("missing executable shell wrapper", result.stderr)

    def test_test_paths_with_spaces_are_run_once(self):
        self.shell()
        (self.root / "tests/jit-test/with spaces.js").write_text("exit 0\n")
        result = self.run_harness()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("night tests: 1 passed, 0 failed", result.stdout)

    def test_shell_failure_fails_the_suite(self):
        self.shell()
        (self.root / "tests/jit-test/failure.js").write_text("exit 7\n")
        result = self.run_harness()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("night tests: 0 passed, 1 failed", result.stdout)


if __name__ == "__main__":
    unittest.main()
