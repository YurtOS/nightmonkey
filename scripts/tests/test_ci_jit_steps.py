"""Run the workflow's jit-test commands against harness-output fixtures."""

import os
import re
import subprocess
import tempfile
import unittest
from pathlib import Path


class JitWorkflowTests(unittest.TestCase):
    def commands(self):
        workflow = (Path(__file__).resolve().parents[2] / ".github/workflows/engine.yml").read_text()
        # Read only literal run commands, with no YAML dependency in this gate.
        for lane in ("AOT", "interpreter"):
            step = workflow.split(f"      - name: jit-tests ({lane})\n", 1)[1]
            step = step.split("\n      - name:", 1)[0]
            run = step.split("        run: ", 1)[1]
            if run.startswith("|\n"):
                run = "\n".join(line[10:] for line in run.splitlines()[1:])
            yield lane, run.strip()

    def run_commands(self, output, status):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "scripts").mkdir()
            (root / "nightmonkey-logs").mkdir()
            harness = root / "scripts/run-jit-tests.sh"
            harness.write_text('#!/usr/bin/env bash\nprintf "%s\\n" "$HARNESS_OUTPUT"\nexit "$HARNESS_STATUS"\n')
            harness.chmod(0o755)
            for lane, command in self.commands():
                with self.subTest(lane=lane, output=output, status=status):
                    result = subprocess.run(
                        ["bash", "-e", "-o", "pipefail", "-c", command],
                        cwd=root, capture_output=True, text=True, check=False,
                        env={**os.environ, "RUNNER_TEMP": directory,
                             "HARNESS_OUTPUT": output, "HARNESS_STATUS": str(status)},
                    )
                    yield result

    def test_no_tests_is_rejected(self):
        for result in self.run_commands("No tests found matching command line arguments.", 0):
            self.assertNotEqual(result.returncode, 0)

    def test_zero_total_is_rejected(self):
        for result in self.run_commands("PASSED ALL\nResult summary:\nPassed: 0\nFailed: 0", 0):
            self.assertNotEqual(result.returncode, 0)

    def test_positive_total_is_accepted(self):
        for result in self.run_commands("PASSED ALL\nResult summary:\nPassed: 2\nFailed: 0", 0):
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_harness_failure_survives_tee(self):
        for result in self.run_commands("Result summary:\nPassed: 2\nFailed: 1", 1):
            self.assertNotEqual(result.returncode, 0)


if __name__ == "__main__":
    unittest.main()
