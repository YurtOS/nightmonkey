"""Exercise the snapshot round-trip script through its process boundary.

The build binaries and the Wasm runner are fakes. The fake `nightmonkey`
writes the kept snapshot and the AOT module from environment variables, and
the fake runner prints a module's contents as its "restored" stdout (or a
fixed text, to isolate the empty-file checks from the output comparison).
Every call is logged so a test can tell whether the runner was reached.
"""

import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
FIXTURE = Path("tests/fixtures/snapshot-roundtrip")

FAKE_NIGHTMONKEY = """\
import os, sys
args = sys.argv[1:]
with open(os.environ["CALLS"], "a") as log:
    log.write("nightmonkey " + " ".join(args) + "\\n")
def value(flag):
    return args[args.index(flag) + 1]
status = int(os.environ.get("NIGHTMONKEY_STATUS", "0"))
if status:
    sys.exit(status)
with open(value("--keep-snapshot"), "w") as f:
    f.write(os.environ["SNAPSHOT_OUTPUT"])
with open(value("-o"), "w") as f:
    f.write(os.environ["AOT_OUTPUT"])
"""

FAKE_RUNNER = """\
import os, sys
args = sys.argv[1:]
with open(os.environ["CALLS"], "a") as log:
    log.write("runner " + " ".join(args) + "\\n")
if "RUNNER_PRINTS" in os.environ:
    sys.stdout.write(os.environ["RUNNER_PRINTS"])
else:
    with open(args[-1]) as f:
        sys.stdout.write(f.read())
sys.exit(int(os.environ.get("RUNNER_STATUS", "0")))
"""


def write_tool(path, body):
    path.write_text(f"#!{sys.executable}\n{body}")
    path.chmod(0o755)


class SnapshotRoundtripTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="snapshot roundtrip ")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        scripts = self.root / "scripts"
        scripts.mkdir()
        source = REPO / "scripts/snapshot-roundtrip.sh"
        if source.exists():
            shutil.copyfile(source, scripts / source.name)
        shutil.copytree(REPO / FIXTURE, self.root / FIXTURE)
        self.expected = (REPO / FIXTURE / "expected.txt").read_text()
        bin_dir = self.root / "build/bin"
        bin_dir.mkdir(parents=True)
        write_tool(bin_dir / "nightmonkey", FAKE_NIGHTMONKEY)
        (bin_dir / "js").write_text("fake wasm shell\n")
        self.runner = self.root / "tools/wasmtime"
        self.runner.parent.mkdir()
        write_tool(self.runner, FAKE_RUNNER)
        self.calls = self.root / "calls"
        # HOME and PATH hold no wasmtime, so only WASMTIME can name a runner.
        empty_path = self.root / "empty-path"
        empty_path.mkdir()
        self.env = {
            "PATH": os.pathsep.join([str(empty_path), "/usr/bin", "/bin"]),
            "HOME": str(self.root / "home"),
            "WASMTIME": str(self.runner),
            "CALLS": str(self.calls),
            "SNAPSHOT_OUTPUT": self.expected,
            "AOT_OUTPUT": self.expected,
        }

    def run_script(self, *args, unset=(), **extra):
        env = {**self.env, **extra}
        for name in unset:
            env.pop(name, None)
        return subprocess.run(
            ["bash", str(self.root / "scripts/snapshot-roundtrip.sh"), *args],
            env=env, capture_output=True, text=True, check=False,
        )

    def call_log(self):
        return self.calls.read_text().splitlines() if self.calls.exists() else []

    def runner_calls(self):
        return [line for line in self.call_log() if line.startswith("runner ")]

    def assert_fails(self, result, code, message, runner_reached):
        self.assertEqual(result.returncode, code, result.stdout + result.stderr)
        self.assertIn(message, result.stderr)
        self.assertEqual(bool(self.runner_calls()), runner_reached, self.call_log())

    def test_passes_when_both_restores_print_the_expected_output(self):
        result = self.run_script()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("snapshot round-trip passed", result.stdout)
        compile_line, snap_line, aot_line = self.call_log()
        program = self.root / FIXTURE / "main.js"
        shell = self.root / "build/bin/js"
        self.assertTrue(compile_line.startswith(
            f"nightmonkey --shell {shell} {program} --keep-snapshot "))
        snapshot = compile_line.split(" --keep-snapshot ")[1].split(" -o ")[0]
        module = compile_line.split(" -o ")[1]
        self.assertEqual(snap_line, f"runner run {snapshot}")
        self.assertEqual(aot_line, f"runner run {module}")

    def test_explicit_build_dir_program_and_expected(self):
        other = self.root / "other build"
        shutil.copytree(self.root / "build", other)
        program = self.root / "prog.js"
        program.write_text("function main() { print('hi'); }\n")
        expected = self.root / "prog.expected"
        expected.write_text("hi\n")
        result = self.run_script(str(other), str(program), str(expected),
                                 SNAPSHOT_OUTPUT="hi\n", AOT_OUTPUT="hi\n")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn(f"nightmonkey --shell {other}/bin/js {program} ", self.call_log()[0])

    def test_restored_snapshot_output_differs(self):
        result = self.run_script(SNAPSHOT_OUTPUT=self.expected + "extra\n")
        self.assert_fails(result, 1, "restored snapshot output differs", True)

    def test_restored_aot_output_differs(self):
        result = self.run_script(AOT_OUTPUT=self.expected.replace("6765", "6766"))
        self.assert_fails(result, 1, "restored AOT module output differs", True)

    def test_runner_failure_with_matching_output_fails(self):
        result = self.run_script(RUNNER_STATUS="3")
        self.assert_fails(result, 1, "exited with status 3", True)

    def test_empty_snapshot_fails_before_restoring(self):
        result = self.run_script(SNAPSHOT_OUTPUT="", RUNNER_PRINTS=self.expected)
        self.assert_fails(result, 1, "snapshot is empty", False)

    def test_empty_aot_module_fails_before_restoring(self):
        result = self.run_script(AOT_OUTPUT="", RUNNER_PRINTS=self.expected)
        self.assert_fails(result, 1, "AOT module is empty", False)

    def test_nightmonkey_failure_fails(self):
        result = self.run_script(NIGHTMONKEY_STATUS="4")
        self.assert_fails(result, 1, "nightmonkey failed", False)

    def test_missing_runner_is_a_setup_failure(self):
        cases = (("unset", {}, ("WASMTIME",)),
                 ("nonexistent", {"WASMTIME": str(self.root / "nonexistent")}, ()))
        for name, extra, unset in cases:
            with self.subTest(runner=name):
                self.calls.unlink(missing_ok=True)
                result = self.run_script(unset=unset, **extra)
                self.assert_fails(result, 2, "wasmtime", False)
                self.assertEqual(self.call_log(), [])

    def test_missing_build_binaries_are_setup_failures(self):
        for name in ("nightmonkey", "js"):
            with self.subTest(binary=name):
                build = self.root / f"build-without-{name}"
                shutil.copytree(self.root / "build", build)
                (build / "bin" / name).unlink()
                self.calls.unlink(missing_ok=True)
                result = self.run_script(str(build))
                self.assert_fails(result, 2, f"{build}/bin/{name}", False)
                self.assertEqual(self.call_log(), [])

    def test_wrong_argument_count_is_a_usage_failure(self):
        result = self.run_script(str(self.root / "build"), "only-a-program.js")
        self.assert_fails(result, 2, "usage:", False)


if __name__ == "__main__":
    unittest.main()
