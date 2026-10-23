"""Exercise the engine-source guard against the real workflow and mutations of it."""

import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
PIN = "d1eae5f68fef9995bfc4b68d1c3451984466fd9d"


class CheckEngineSourceTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="engine source ")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / "scripts").mkdir()
        shutil.copyfile(REPO / "scripts/check-engine-source.sh", self.root / "scripts/check-engine-source.sh")
        (self.root / ".github/workflows").mkdir(parents=True)
        (self.root / "engine").mkdir()
        self.workflow = (REPO / ".github/workflows/engine.yml").read_text()

    def check(self, workflow=None, pin=(PIN + "\n").encode()):
        (self.root / ".github/workflows/engine.yml").write_text(self.workflow if workflow is None else workflow)
        pin_file = self.root / "engine/QUEUE_PIN"
        if pin is None:
            pin_file.unlink(missing_ok=True)
        else:
            pin_file.write_bytes(pin)
        return subprocess.run(
            ["bash", str(self.root / "scripts/check-engine-source.sh")],
            capture_output=True, text=True, check=False,
        )

    def mutate(self, old, new, count=1):
        self.assertEqual(self.workflow.count(old), count, f"fixture text not found once: {old!r}")
        return self.workflow.replace(old, new)

    def assert_rejected(self, result, reason):
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertNotIn("check-engine-source: ok", result.stdout)
        self.assertIn(reason, result.stderr)

    def test_repository_workflow_and_pin_pass(self):
        result = self.check(pin=(REPO / "engine/QUEUE_PIN").read_bytes())
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("check-engine-source: ok", result.stdout)

    def test_fixture_pin_passes(self):
        result = self.check()
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_checkout_on_a_branch_is_rejected(self):
        self.assert_rejected(self.check(self.mutate("ref: ${{ steps.pin.outputs.sha }}", "ref: main")),
                             "ref: ${{ steps.pin.outputs.sha }}")

    def test_missing_pin_step_is_rejected(self):
        start = self.workflow.index("      - name: Read the engine queue pin\n")
        end = self.workflow.index("      - name: Check out the engine queue\n")
        workflow = self.workflow[:start] + self.workflow[end:]
        self.assert_rejected(self.check(workflow), "id: pin")

    def test_pin_step_that_does_not_read_the_pin_file_is_rejected(self):
        self.assert_rejected(self.check(self.mutate("pin=$(cat engine/QUEUE_PIN)", "pin=$(git ls-remote https://github.com/YurtOS/yurt-firefox main | cut -f1)")),
                             "engine/QUEUE_PIN")

    def test_build_from_the_old_script_is_rejected(self):
        workflow = self.mutate('engine-src/scripts/build-engine.sh --workdir "$GITHUB_WORKSPACE/firefox"',
                               "scripts/build-spidermonkey.sh firefox")
        self.assert_rejected(self.check(workflow), "build-engine.sh")

    def test_build_only_in_a_comment_is_rejected(self):
        workflow = self.mutate('          engine-src/scripts/build-engine.sh --workdir "$GITHUB_WORKSPACE/firefox"\n',
                               '          # engine-src/scripts/build-engine.sh --workdir "$GITHUB_WORKSPACE/firefox"\n'
                               '          true\n')
        self.assert_rejected(self.check(workflow), "build-engine.sh")

    def test_fork_repository_is_rejected(self):
        workflow = self.mutate("repository: YurtOS/yurt-firefox", "repository: YurtOS/firefox")
        self.assert_rejected(self.check(workflow), "YurtOS/firefox")

    def test_fork_env_is_rejected(self):
        workflow = self.mutate("  SM_OBJDIR: obj-nightmonkey-sm\n",
                               "  SM_OBJDIR: obj-nightmonkey-sm\n  FIREFOX_REF: yurt\n")
        self.assert_rejected(self.check(workflow), "fork")

    def test_missing_ancestry_check_is_rejected(self):
        workflow = self.mutate("merge-base --is-ancestor", "merge-base")
        self.assert_rejected(self.check(workflow), "ancestor")

    def test_swallowed_ancestry_check_is_rejected(self):
        line = next(l for l in self.workflow.splitlines() if "merge-base --is-ancestor" in l)
        workflow = self.mutate(line, line.split("||")[0] + "|| true")
        self.assert_rejected(self.check(workflow), "ancestor")

    def test_missing_main_fetch_is_rejected(self):
        line = next(l for l in self.workflow.splitlines() if "git -C engine-src fetch" in l)
        self.assert_rejected(self.check(self.mutate(line + "\n", "")), "fetch")

    def test_missing_mozconfig_comparison_is_rejected(self):
        line = next(l for l in self.workflow.splitlines() if "cmp spidermonkey/mozconfig engine-src/mozconfig" in l)
        self.assert_rejected(self.check(self.mutate(line + "\n", "")), "mozconfig")

    def test_unpinned_action_is_rejected(self):
        line = next(l for l in self.workflow.splitlines() if "uses: actions/upload-artifact@" in l)
        workflow = self.mutate(line, line.split("@")[0] + "@v4")
        self.assert_rejected(self.check(workflow), "full commit SHA")

    def test_missing_manifest_copy_is_rejected(self):
        line = next(l for l in self.workflow.splitlines() if "build-manifest.json" in l and "cp " in l)
        self.assert_rejected(self.check(self.mutate(line + "\n", "")), "build-manifest.json")

    def test_build_before_the_checks_is_rejected(self):
        build = "      - name: Build the engine (patched SpiderMonkey)\n"
        start = self.workflow.index(build)
        end = self.workflow.index("\n\n", start) + 2
        step = self.workflow[start:end]
        workflow = self.workflow[:start] + self.workflow[end:]
        anchor = "      - name: Check out the engine queue\n"
        workflow = workflow.replace(anchor, step + anchor)
        self.assert_rejected(self.check(workflow), "order")

    def test_bad_pins_are_rejected(self):
        for label, pin in (
            ("crlf", (PIN + "\r\n").encode()),
            ("39 hex", (PIN[:39] + "\n").encode()),
            ("41 hex", (PIN + "0\n").encode()),
            ("upper case", (PIN.upper() + "\n").encode()),
            ("no newline", PIN.encode()),
            ("two lines", (PIN + "\n" + PIN + "\n").encode()),
            ("empty", b""),
        ):
            with self.subTest(pin=label):
                self.assert_rejected(self.check(pin=pin), "engine/QUEUE_PIN")

    def test_missing_pin_file_is_rejected(self):
        self.assert_rejected(self.check(pin=None), "engine/QUEUE_PIN is missing")


if __name__ == "__main__":
    unittest.main()
