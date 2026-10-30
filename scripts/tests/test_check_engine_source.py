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

    def check(self, workflow: str | None = None, pin: bytes | None = (PIN + "\n").encode()):
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

    def add_to_step(self, name, line):
        """Insert a key line right after the step's `- name:` line."""
        return self.mutate(f"      - name: {name}\n", f"      - name: {name}\n        {line}\n")

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
        # Every copy of the line (engine.yml uploads logs and shard inputs).
        workflow = self.mutate(line, line.split("@")[0] + "@v4", count=self.workflow.count(line))
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

    # Round 2: a check step switched off, made non-blocking or rewritten.
    STEPS = {
        "pin": "Read the engine queue pin",
        "checkout": "Check out the engine queue",
        "ancestry": "Verify the engine queue pin is on yurt-firefox main",
        "mozconfig": "Check spidermonkey/mozconfig matches the engine queue",
        "build": "Build the engine (patched SpiderMonkey)",
    }

    def test_disabled_or_non_blocking_steps_are_rejected(self):
        for step, name in self.STEPS.items():
            for line in ("if: false", "if: ${{ github.event_name == 'never' }}", "continue-on-error: true"):
                with self.subTest(step=step, line=line):
                    self.assert_rejected(self.check(self.add_to_step(name, line)), name)

    def test_job_level_if_or_continue_on_error_is_rejected(self):
        for line in ("    if: false\n", "    continue-on-error: true\n"):
            with self.subTest(line=line):
                workflow = self.mutate("    runs-on: ubuntu-24.04\n", "    runs-on: ubuntu-24.04\n" + line)
                self.assert_rejected(self.check(workflow), "job-level " + line.split(":")[0].strip())

    def test_fetch_from_another_remote_is_rejected(self):
        workflow = self.mutate("git -C engine-src fetch --no-tags origin +refs",
                               "git -C engine-src fetch --no-tags https://github.com/evil/yurt-firefox +refs")
        self.assert_rejected(self.check(workflow), self.STEPS["ancestry"])

    def test_extra_refspec_is_rejected(self):
        workflow = self.mutate("--no-tags origin +refs/heads/main:refs/remotes/origin/main",
                               "--no-tags origin +refs/heads/evil:refs/remotes/origin/main +refs/heads/main:refs/remotes/origin/main")
        self.assert_rejected(self.check(workflow), self.STEPS["ancestry"])

    def test_lines_before_the_ancestry_check_are_rejected(self):
        line = next(l for l in self.workflow.splitlines() if "merge-base --is-ancestor" in l)
        indent = line[:len(line) - len(line.lstrip())]
        for extra in ("exit 0", "git -C engine-src update-ref refs/remotes/origin/main HEAD"):
            with self.subTest(extra=extra):
                workflow = self.mutate(line, indent + extra + "\n" + line)
                self.assert_rejected(self.check(workflow), self.STEPS["ancestry"])

    def test_second_sha_output_is_rejected(self):
        line = '          echo "sha=$pin" >> "$GITHUB_OUTPUT"\n'
        workflow = self.mutate(line, line + '          echo "sha=0123456789abcdef0123456789abcdef01234567" >> "$GITHUB_OUTPUT"\n')
        self.assert_rejected(self.check(workflow), self.STEPS["pin"])

    def test_extra_engine_build_step_is_rejected(self):
        anchor = "      - uses: Swatinem/rust-cache@"
        workflow = self.mutate(anchor, "      - name: Rebuild\n        run: (cd firefox && ./mach build)\n\n" + anchor)
        self.assert_rejected(self.check(workflow), "Rebuild")

    def test_other_step_touching_engine_src_is_rejected(self):
        anchor = "      - name: Build the engine (patched SpiderMonkey)\n"
        workflow = self.mutate(anchor, "      - name: Adjust\n        run: git -C engine-src checkout -q HEAD~1\n\n" + anchor)
        self.assert_rejected(self.check(workflow), "Adjust")

    # Round 3: key order inside a step, and plain diagnostics.
    def first_step(self, text):
        return self.mutate("    steps:\n", "    steps:\n" + text)

    def test_step_with_run_before_name_is_rejected(self):
        workflow = self.first_step("      - run: scripts/build-spidermonkey.sh firefox\n"
                                   "        name: Rebuild engine\n")
        self.assert_rejected(self.check(workflow), "Rebuild engine")

    def test_unnamed_step_touching_engine_src_is_rejected(self):
        workflow = self.first_step("      - run: git -C engine-src checkout -q HEAD~1\n")
        self.assert_rejected(self.check(workflow), "git -C engine-src checkout")

    # The guard is conservative: any mention of engine-src, mach, build-engine.sh
    # or build-spidermonkey.sh outside the required steps is rejected, plain
    # echo/printf text included (an exemption for "printing" lines was bypassed
    # by continuations, folded scalars, heredocs and YAML data). Such text goes
    # in a required step or out of the workflow.
    def test_plain_diagnostics_are_rejected_conservatively(self):
        df = "          df -h /\n"
        for label, workflow in (
            ("echo mach", self.mutate(df, df + '          echo "Disk space ready for mach"\n')),
            ("echo engine-src", self.mutate(df, df + "          echo 'engine-src holds the queue checkout'\n")),
            ("printf", self.mutate(df, df + '          printf "%s\\n" "build-engine.sh runs next"\n')),
            ("one-line run", self.first_step('      - name: Note\n        run: echo "mach toolchains next"\n\n')),
        ):
            with self.subTest(case=label):
                self.assert_rejected(self.check(workflow), "move the text out of non-critical steps")

    def test_echo_that_acts_on_engine_src_is_rejected(self):
        df = "          df -h /\n"
        for extra in ("echo x > engine-src/mozconfig",
                      'echo "$(cd engine-src && git log -1)"',
                      "echo build | tee engine-src/x",
                      "echo ok; ./mach build"):
            with self.subTest(extra=extra):
                workflow = self.mutate(df, df + "          " + extra + "\n")
                self.assert_rejected(self.check(workflow), "Free disk space")

    # Round 4: the printing-line exemption's bypasses, now caught by the plain rule.
    def test_backslash_continuation_onto_echo_is_rejected(self):
        df = "          df -h /\n"
        workflow = self.mutate(df, df + "          rm -rf \\\n          echo engine-src\n")
        self.assert_rejected(self.check(workflow), "Free disk space")

    def test_folded_scalar_is_rejected(self):
        workflow = self.first_step("      - name: Fold\n        run: >\n          rm -rf\n          echo engine-src\n\n")
        self.assert_rejected(self.check(workflow), "Fold")

    def test_heredoc_line_starting_with_echo_is_rejected(self):
        workflow = self.first_step("      - name: Py\n        run: |\n          python3 - <<'PYX'\n"
                                   "          echo = __import__(\"shutil\").rmtree(\"engine-src\")\n"
                                   "          PYX\n\n")
        self.assert_rejected(self.check(workflow), "Py")

    def test_yaml_data_named_echo_is_rejected(self):
        checkout = next(l for l in self.workflow.splitlines() if "uses: actions/checkout@" in l).split("uses: ")[1]
        for label, step in (
            ("env key", "      - name: Env\n        env:\n          echo: engine-src\n"
                        "        run: rm -rf \"${{ env.echo }}\"\n\n"),
            ("with path", "      - name: Extra checkout\n        uses: " + checkout + "\n        with:\n"
                          "          path: >-\n            echo/../engine-src\n\n"),
        ):
            with self.subTest(case=label):
                self.assert_rejected(self.check(self.first_step(step)), step.split("name: ")[1].split("\n")[0])

    def test_missing_pin_file_is_rejected(self):
        self.assert_rejected(self.check(pin=None), "engine/QUEUE_PIN is missing")


if __name__ == "__main__":
    unittest.main()
