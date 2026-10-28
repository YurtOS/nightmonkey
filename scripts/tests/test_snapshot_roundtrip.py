"""Exercise the snapshot round-trip script through its process boundary.

The build binaries and the Wasm runner are fakes. The fake `nightmonkey`
writes the kept snapshot and the AOT module from environment variables, and
the fake runner prints a module's contents as its "restored" stdout (or a
fixed text, to isolate the empty-file checks from the output comparison).
Every call is logged so a test can tell whether the runner was reached.
"""

import json
import os
import shlex
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
import json, os, sys
args = sys.argv[1:]
with open(os.environ["CALLS"], "a") as log:
    log.write("LABEL " + " ".join(args) + "\\n")
with open(os.environ["RUNNER_ARGV"], "a") as log:
    log.write(json.dumps(args) + "\\n")
if "RUNNER_PRINTS" in os.environ:
    sys.stdout.write(os.environ["RUNNER_PRINTS"])
else:
    with open(args[-1]) as f:
        sys.stdout.write(f.read())
sys.exit(int(os.environ.get("RUNNER_STATUS", "0")))
"""


# Commands the script runs besides shell builtins; the tests' PATH holds only
# these, so no host wasmtime can be found.
SCRIPT_UTILITIES = ("basename", "cmp", "diff", "dirname", "mktemp", "rm")


def write_tool(path, body):
    """An executable fake: a /bin/sh launcher running the Python body.

    A shebang naming sys.executable directly breaks when that path (or the
    temp dir) contains spaces, so the launcher quotes both.
    """
    script = path.with_name(path.name + ".py")
    script.write_text(body)
    path.write_text(f'#!/bin/sh\nexec {shlex.quote(sys.executable)} "$0.py" "$@"\n')
    path.chmod(0o755)


def write_runner(path, label="runner"):
    write_tool(path, FAKE_RUNNER.replace("LABEL", label))


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
        write_runner(self.runner)
        self.calls = self.root / "calls"
        # HOME and PATH hold no wasmtime, so only WASMTIME can name a runner.
        self.path_dir = self.root / "path"
        self.path_dir.mkdir()
        for name in SCRIPT_UTILITIES:
            found = shutil.which(name)
            self.assertIsNotNone(found, name)
            (self.path_dir / name).symlink_to(found)
        self.bash = shutil.which("bash")
        self.env = {
            "PATH": str(self.path_dir),
            "HOME": str(self.root / "home"),
            "WASMTIME": str(self.runner),
            "CALLS": str(self.calls),
            "RUNNER_ARGV": str(self.root / "runner-argv"),
            "SNAPSHOT_OUTPUT": self.expected,
            "AOT_OUTPUT": self.expected,
        }

    def run_script(self, *args, unset=(), cwd=None, **extra):
        env = {**self.env, **extra}
        for name in unset:
            env.pop(name, None)
        return subprocess.run(
            [self.bash, str(self.root / "scripts/snapshot-roundtrip.sh"), *args],
            env=env, capture_output=True, text=True, check=False, cwd=cwd,
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

    def test_missing_program_or_expected_is_a_setup_failure(self):
        program = self.root / FIXTURE / "main.js"
        expected = self.root / FIXTURE / "expected.txt"
        absent = self.root / "absent"
        for name, args, message in (
                ("program", (str(absent), str(expected)), f"missing program {absent}"),
                ("expected", (str(program), str(absent)),
                 f"missing expected output {absent}")):
            with self.subTest(missing=name):
                self.calls.unlink(missing_ok=True)
                result = self.run_script(str(self.root / "build"), *args)
                self.assert_fails(result, 2, message, False)
                self.assertEqual(self.call_log(), [])

    def runner_argvs(self):
        log = self.root / "runner-argv"
        lines = log.read_text().splitlines() if log.exists() else []
        return [json.loads(line) for line in lines]

    def assert_runner_flags(self, flags, expected, **extra):
        (self.root / "runner-argv").unlink(missing_ok=True)
        result = self.run_script(WASMTIME_FLAGS=flags, **extra)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        argvs = self.runner_argvs()
        self.assertEqual(len(argvs), 2, argvs)
        for argv in argvs:
            self.assertEqual(argv[:-1], ["run", *expected], argv)
            self.assertTrue(argv[-1].endswith(".wasm"), argv)

    def test_runner_flags_reach_the_runner_as_separate_arguments(self):
        self.assert_runner_flags("-W unknown-imports-trap", ["-W", "unknown-imports-trap"])

    def test_runner_flags_split_on_newlines_without_globbing(self):
        # cwd holds files, so an unquoted, globbed `*` would expand.
        self.assert_runner_flags("-W unknown-imports-trap\n\t--env  X=*\n*\n",
                                 ["-W", "unknown-imports-trap", "--env", "X=*", "*"],
                                 cwd=self.root)

    def test_empty_runner_flags_add_no_arguments(self):
        for flags in ("", " \n\t"):
            with self.subTest(flags=repr(flags)):
                self.assert_runner_flags(flags, [])

    def test_runner_search_order(self):
        on_path = self.path_dir / "wasmtime"
        in_home = self.root / "home/bin/wasmtime"
        in_home.parent.mkdir(parents=True)
        write_runner(in_home, "home-runner")
        with self.subTest(found="home"):
            result = self.run_script(unset=("WASMTIME",))
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertEqual([l.split()[0] for l in self.call_log()[1:]],
                             ["home-runner"] * 2)
        write_runner(on_path, "path-runner")
        with self.subTest(found="path before home"):
            self.calls.unlink()
            result = self.run_script(unset=("WASMTIME",))
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertEqual([l.split()[0] for l in self.call_log()[1:]],
                             ["path-runner"] * 2)
        with self.subTest(found="WASMTIME before path"):
            self.calls.unlink()
            result = self.run_script()
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertEqual([l.split()[0] for l in self.call_log()[1:]], ["runner"] * 2)

    def test_relative_build_dir_with_cdpath(self):
        result = self.run_script("build", cwd=self.root, CDPATH=".")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        # The relative build dir resolves against the physical cwd (macOS
        # temp dirs sit behind /var -> /private/var); fold that back.
        line = self.call_log()[0].replace(str(self.root.resolve()), str(self.root))
        self.assertTrue(line.startswith(
            f"nightmonkey --shell {self.root}/build/bin/js {self.root / FIXTURE}/main.js "),
            self.call_log())

    def test_wrong_argument_count_is_a_usage_failure(self):
        build = str(self.root / "build")
        program = str(self.root / FIXTURE / "main.js")
        expected = str(self.root / FIXTURE / "expected.txt")
        for args in ((build, program), (build, program, expected, "extra")):
            with self.subTest(count=len(args)):
                self.calls.unlink(missing_ok=True)
                result = self.run_script(*args)
                self.assert_fails(result, 2, "usage:", False)
                self.assertEqual(self.call_log(), [])


if __name__ == "__main__":
    unittest.main()
