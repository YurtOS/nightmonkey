"""Exercise lane orchestration through its shell/process boundary."""

import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path


class CiSuiteTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="ci suite ")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        scripts = self.root / "scripts"
        scripts.mkdir()
        source = Path(__file__).resolve().parents[1] / "ci-suite.sh"
        if source.exists():
            shutil.copyfile(source, scripts / source.name)
        # The timeout boundary records the cap and simulates a timed-out AOT
        # process. This tests orchestration, not coreutils' timer internals.
        tools = self.root / "tools"
        tools.mkdir()
        timeout = tools / "timeout"
        timeout.write_text(
            '#!/usr/bin/env bash\nprintf "%s %s\\n" "$1" "$2" >> "$CAPS"\n'
            'shift 2\nexec "$@"\n'
        )
        timeout.chmod(0o755)
        for name in ("run-night-tests.sh", "run-jit-tests.sh", "run-jstests.sh"):
            harness = scripts / name
            harness.write_text(
                '#!/usr/bin/env bash\nprintf "%s\\n" "$NIGHT_INPROCESS_OFF" >> "$LANES"\n'
                'printf "%s\\n" "$1" >> "$ROOTS"\n'
                'if [ "$NIGHT_INPROCESS_OFF" = 0 ]; then exit "${AOT_STATUS:-0}"; fi\n'
                'exit "${INTERPRETER_STATUS:-0}"\n'
            )
            harness.chmod(0o755)
        self.env = {**os.environ, "PATH": str(tools) + os.pathsep + os.environ["PATH"],
                    "RUNNER_TEMP": str(self.root), "CAPS": str(self.root / "caps"),
                    "LANES": str(self.root / "lanes"), "ROOTS": str(self.root / "roots")}
        self.env.pop("NIGHT_FIREFOX_TESTS", None)

    def run_suite(self, suite, lane, **extra):
        return subprocess.run(
            ["bash", str(self.root / "scripts/ci-suite.sh"), suite, lane],
            env={**self.env, **extra}, capture_output=True, text=True, check=False,
        )

    def test_single_lanes_set_the_engine_switch(self):
        for lane, switch in (("aot", "0"), ("interpreter", "1")):
            with self.subTest(lane=lane):
                (self.root / "lanes").unlink(missing_ok=True)
                result = self.run_suite("local", lane)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual((self.root / "lanes").read_text(), switch + "\n")

    def test_both_runs_interpreter_after_aot_timeout(self):
        result = self.run_suite("jit", "both", AOT_STATUS="124")
        self.assertNotEqual(result.returncode, 0)
        self.assertTrue((self.root / "lanes").exists(), result.stderr)
        self.assertEqual((self.root / "lanes").read_text(), "0\n1\n")
        self.assertTrue((self.root / "nightmonkey-logs/jit-aot.log").exists())
        self.assertTrue((self.root / "nightmonkey-logs/jit-interpreter.log").exists())

    def test_interpreter_failure_fails_both(self):
        result = self.run_suite("local", "both", INTERPRETER_STATUS="9")
        self.assertNotEqual(result.returncode, 0)
        self.assertTrue((self.root / "lanes").exists(), result.stderr)
        self.assertEqual((self.root / "lanes").read_text(), "0\n1\n")

    def test_per_lane_caps_are_applied_to_each_requested_process(self):
        for suite, seconds in (("local", 1800), ("jit", 3600), ("jstests", 7200)):
            with self.subTest(suite=suite):
                (self.root / "caps").unlink(missing_ok=True)
                result = self.run_suite(suite, "both")
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual((self.root / "caps").read_text(), f"--kill-after=30s {seconds}s\n" * 2)

    def test_test_source_root_comes_from_the_environment(self):
        bundle = self.root / "testsrc"
        cases = (("jit", {}, self.root / "firefox"), ("jstests", {}, self.root / "firefox"),
                 ("jit", {"NIGHT_FIREFOX_TESTS": str(bundle)}, bundle),
                 ("jstests", {"NIGHT_FIREFOX_TESTS": str(bundle)}, bundle),
                 ("local", {"NIGHT_FIREFOX_TESTS": str(bundle)}, self.root / "build"))
        for suite, extra, expected in cases:
            with self.subTest(suite=suite, env=extra):
                (self.root / "roots").unlink(missing_ok=True)
                result = self.run_suite(suite, "aot", **extra)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual((self.root / "roots").read_text(), f"{expected}\n")

    def test_invalid_contract_values_are_setup_failures(self):
        for suite, lane in (("unknown", "aot"), ("jit", "unknown")):
            with self.subTest(suite=suite, lane=lane):
                self.assertEqual(self.run_suite(suite, lane).returncode, 2)
                self.assertFalse((self.root / "lanes").exists())


if __name__ == "__main__":
    unittest.main()
