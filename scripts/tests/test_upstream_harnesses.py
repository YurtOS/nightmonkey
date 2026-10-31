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
        (lib / "manifest.py").write_text("def _find_all_js_files(location):\n    yield from ()\n")

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

    def test_jstests_random_results_do_not_distort_execution_count(self):
        result = self.run_wrapper("js", "TEST-PASS | normal.js\nTEST-PASS (EXPECTED RANDOM) | random.js\nNIGHT-JSTESTS completed=1 passed=1 failed=0 timeout=0 skipped=1")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_jstests_random_only_run_is_rejected(self):
        result = self.run_wrapper("js", "TEST-PASS (EXPECTED RANDOM) | random.js\nNIGHT-JSTESTS completed=1 passed=0 failed=0 timeout=0 skipped=1")
        self.assertEqual(result.returncode, 2)

    def test_completion_adapter_reports_real_callback(self):
        harness = self.firefox / "js/src/tests/jstests.py"
        harness.write_text("from lib.results import ResultsSink\nimport os\ns = ResultsSink()\ns.counts = {'PASS': 1, 'FAIL': 0, 'TIMEOUT': 0, 'SKIP': 0}\nprint('TEST-PASS | normal.js')\ns.finish(os.environ['HARNESS_OUTPUT'] == 'complete')\n")
        self.assertEqual(self.run_wrapper("js", "complete").returncode, 0)
        self.assertEqual(self.run_wrapper("js", "interrupted").returncode, 2)

    def test_jstests_passes_the_engine_build_info(self):
        # Upstream jstests.py exits 1 before running anything unless it finds
        # config/autoconf.mk above the shell or gets --xul-info=ABI:OS:DEBUG;
        # NightMonkey's shell lives in build/bin, outside the objdir.
        harness = self.firefox / "js/src/tests/jstests.py"
        harness.write_text(
            "import sys\n"
            "info = [a.split('=', 1)[1] for a in sys.argv if a.startswith('--xul-info=')]\n"
            "if not info:\n"
            "    print(\"Can't find config/autoconf.mk on a directory containing the JS shell\")\n"
            "    sys.exit(1)\n"
            "print('XUL-INFO ' + info[0])\n"
            "print('TEST-PASS | example.js')\n"
            "print('NIGHT-JSTESTS completed=1 passed=1 failed=0 timeout=0 skipped=0')\n"
        )
        result = self.run_wrapper("js", "")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        # What obj-nightmonkey-sm/config/autoconf.mk records for
        # spidermonkey/mozconfig: TARGET_XPCOM_ABI, OS_TARGET, MOZ_DEBUG.
        self.assertIn("XUL-INFO wasm32-gcc3:WASI:false\n", result.stdout)

    def test_the_default_xul_info_is_the_release_bundles(self):
        # The default (for a Firefox checkout) is what the engine release's
        # jstests bundle records in xul-info.txt (engine-v147.0.4-1:
        # wasm32-gcc3:WASI:false, from the objdir of yurt-firefox's
        # mozconfig); a bundle's own xul-info.txt wins over it.
        repo = Path(__file__).resolve().parents[2]
        self.assertIn("xul_info=wasm32-gcc3:WASI:false\n", (repo / "scripts/run-jstests.sh").read_text())
        self.assertNotIn("spidermonkey/mozconfig", (repo / "scripts/run-jstests.sh").read_text())

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



class JstestsXulInfoTests(unittest.TestCase):
    """jstests.py takes --xul-info instead of reading config/autoconf.mk. The
    jstests asset of an engine release ships xul-info.txt at its root (read
    from the objdir that built it), and run-jstests.sh passes that; a Firefox
    checkout (no xul-info.txt) keeps the value run-jstests.sh records for
    spidermonkey/mozconfig."""

    def setUp(self):
        UpstreamHarnessTests.setUp(self)  # the same fixture, not its cases
        (self.firefox / "js/src/tests/jstests.py").write_text(
            'import os, sys\nopen(os.environ["ARGV_OUT"], "w").write("\\n".join(sys.argv[1:]))\n'
            'print(os.environ["HARNESS_OUTPUT"])\n')
        self.argv = self.root / "argv"

    def run_js(self):
        return subprocess.run(
            ["bash", str(self.scripts / "run-jstests.sh"), str(self.firefox), str(self.build)],
            env={**os.environ, "ARGV_OUT": str(self.argv),
                 "HARNESS_OUTPUT": "TEST-PASS | example.js\nNIGHT-JSTESTS completed=1 passed=1 failed=0 timeout=0 skipped=0"},
            capture_output=True, text=True, check=False)

    def test_a_bundle_passes_its_xul_info(self):
        (self.firefox / "xul-info.txt").write_text("wasm32-clang:WASI:false\n")
        result = self.run_js()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        argv = self.argv.read_text().split("\n")
        self.assertEqual([a for a in argv if a.startswith("--xul-info")], ["--xul-info=wasm32-clang:WASI:false"])

    def test_a_checkout_keeps_the_recorded_value(self):
        result = self.run_js()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        argv = self.argv.read_text().split("\n")
        self.assertEqual([a for a in argv if a.startswith("--xul-info")], ["--xul-info=wasm32-gcc3:WASI:false"])

    def test_an_unusable_xul_info_file_fails(self):
        for text in ("", "a:b\n", "a:b:c:d\n", "wasm32-clang:WASI:false\nextra\n"):
            with self.subTest(text=text):
                (self.firefox / "xul-info.txt").write_text(text)
                result = self.run_js()
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("xul-info.txt", result.stderr)


if __name__ == "__main__":
    unittest.main()
