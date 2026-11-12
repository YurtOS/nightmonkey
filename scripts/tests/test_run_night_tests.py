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
        wrapper.parent.mkdir(parents=True, exist_ok=True)
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

    def exclusion_run(self, version, cache=True):
        self.shell()
        (self.root / "tests/jit-test/ok.js").write_text("exit 0\n")
        (self.root / "tests/jit-test/sub").mkdir(exist_ok=True)
        (self.root / "tests/jit-test/sub/needs-hook.js").write_text("exit 9\n")
        (self.root / "tests/night-tests-excludes-ff147.txt").write_text(
            "# a comment\n\nsub/needs-hook.js\n")
        if cache:
            (self.root / "build/CMakeCache.txt").write_text(
                f"NIGHT_ENGINE_VERSION:STRING={version}\n")
        return self.run_harness()

    def test_version_exclusions_skip_tests_only_for_that_engine(self):
        # A test one engine cannot pass is skipped on that engine's builds and
        # still runs (and fails here) on any other, or with no CMake cache.
        skipped = self.exclusion_run("ff147")
        self.assertEqual(skipped.returncode, 0, skipped.stdout + skipped.stderr)
        self.assertIn("night tests: 1 passed, 0 failed, 1 skipped for engine ff147", skipped.stdout)
        for version, cache in (("ff159", True), (None, False)):
            with self.subTest(version=version):
                other = self.exclusion_run(version, cache)
                self.assertNotEqual(other.returncode, 0)
                self.assertIn("night tests: 1 passed, 1 failed", other.stdout)


if __name__ == "__main__":
    unittest.main()
