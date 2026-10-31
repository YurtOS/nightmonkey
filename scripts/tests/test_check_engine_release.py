"""Exercise the engine-release guard against the real workflow and mutations of it."""

import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
TAG = "engine-v147.0.4-1"
DIGESTS = {
    "manifest": "1" * 64,
    "wasm32-wasi": "2" * 64,
    "testsrc": "3" * 64,
    "jstests": "4" * 64,
    "toolchain-clang": "5" * 64,
    "toolchain-sysroot": "6" * 64,
}


def release_file(tag=TAG, digests=None, extra=""):
    digests = dict(DIGESTS if digests is None else digests)
    lines = ["# fixture", f"tag={tag}"] + [f"{k}={v}" for k, v in digests.items()]
    return "\n".join(lines) + "\n" + extra


CHECKOUT = "actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1"


class CheckEngineReleaseTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="engine release ")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / "scripts").mkdir()
        shutil.copyfile(REPO / "scripts/check-engine-release.sh", self.root / "scripts/check-engine-release.sh")
        (self.root / ".github/workflows").mkdir(parents=True)
        (self.root / "engine").mkdir()
        self.workflow = (REPO / ".github/workflows/engine-release.yml").read_text()

    def check(self, workflow=None, release: "str | None" = release_file()):
        (self.root / ".github/workflows/engine-release.yml").write_text(self.workflow if workflow is None else workflow)
        f = self.root / "engine/ENGINE_RELEASE"
        if release is None:
            f.unlink(missing_ok=True)
        else:
            f.write_text(release)
        return subprocess.run(["bash", str(self.root / "scripts/check-engine-release.sh")],
                              capture_output=True, text=True, check=False)

    def mutate(self, old, new, count=1):
        self.assertEqual(self.workflow.count(old), count, f"fixture text not found once: {old!r}")
        return self.workflow.replace(old, new)

    def assert_rejected(self, result, reason):
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertNotIn("check-engine-release: ok", result.stdout)
        self.assertIn(reason, result.stderr)

    # --- the repository's own files -------------------------------------

    def test_repository_workflow_passes_with_a_valid_release_file(self):
        result = self.check()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("check-engine-release: ok", result.stdout)

    def test_repository_release_file(self):
        """Until the first release is published, engine/ENGINE_RELEASE holds the
        PENDING placeholder and the guard (so CI) refuses it; once filled in
        from the release's MANIFEST.json it must pass."""
        text = (REPO / "engine/ENGINE_RELEASE").read_text()
        result = self.check(release=text)
        if "tag=PENDING\n" in text:
            self.assert_rejected(result, "PENDING")
        else:
            self.assertEqual(result.returncode, 0, result.stderr)

    # --- engine/ENGINE_RELEASE --------------------------------------------

    def test_floating_or_placeholder_tags_are_rejected(self):
        for tag in ("latest", "PENDING", "engine-v147.0.4", "engine-v147.0.4-0", "v147.0.4-1",
                    "engine-v147.0.4-1 ", "engine-v*", "main", ""):
            with self.subTest(tag=tag):
                self.assert_rejected(self.check(release=release_file(tag=tag)), "tag")

    def test_every_digest_is_required(self):
        for key in DIGESTS:
            with self.subTest(missing=key):
                d = dict(DIGESTS)
                del d[key]
                self.assert_rejected(self.check(release=release_file(digests=d)), key)

    def test_digests_are_full_lowercase_sha256(self):
        for bad in ("PENDING", "2" * 63, "A" * 64, "2" * 64 + " ", ""):
            with self.subTest(digest=bad):
                d = dict(DIGESTS, testsrc=bad)
                self.assert_rejected(self.check(release=release_file(digests=d)), "testsrc")

    def test_unknown_and_duplicate_keys_are_rejected(self):
        self.assert_rejected(self.check(release=release_file(extra="extra=" + "7" * 64 + "\n")), "extra")
        self.assert_rejected(self.check(release=release_file(extra="testsrc=" + "3" * 64 + "\n")), "testsrc appears 2 times")
        self.assert_rejected(self.check(release=release_file(extra="tag=" + TAG + "\n")), "tag")

    def test_missing_release_file_is_rejected(self):
        self.assert_rejected(self.check(release=None), "engine/ENGINE_RELEASE")

    # --- the workflow ----------------------------------------------------

    def test_the_fetch_step_must_download_the_jstests_asset_when_asked(self):
        self.assert_rejected(self.check(self.mutate('--jstests "$FULL_JSTESTS"', "--jstests false")),
                             "Fetch the engine release")
        self.assert_rejected(self.check(self.mutate("FULL_JSTESTS: ${{ inputs.full_jstests || inputs.jstests_inputs }}",
                                                    "FULL_JSTESTS: false")), "Fetch the engine release")
        # The jstests inputs for jstests-aot.yml's shards need the jstests asset too.
        self.assert_rejected(self.check(self.mutate("FULL_JSTESTS: ${{ inputs.full_jstests || inputs.jstests_inputs }}",
                                                    "FULL_JSTESTS: ${{ inputs.full_jstests }}")), "Fetch the engine release")

    def test_the_fetch_step_text_is_exact(self):
        self.assert_rejected(
            self.check(self.mutate("scripts/fetch-engine-release.sh", "scripts/fetch-engine-release.sh --no-verify")),
            "Fetch the engine release")
        self.assert_rejected(self.check(self.mutate("      - name: Local regressions\n",
                                                    "      - name: Local regressions\n        continue-on-error: true\n")),
                             "uses continue-on-error")
        self.assert_rejected(self.check(self.mutate("      - name: Fetch the engine release\n",
                                                    "      - name: Fetch the engine release\n        if: ${{ inputs.lane == 'x' }}\n")),
                             "Fetch the engine release")

    def test_the_jstests_step_is_exact(self):
        jstests_if = "        if: ${{ !cancelled() && steps.build.outcome == 'success' && inputs.full_jstests }}\n"
        self.assert_rejected(self.check(self.mutate(jstests_if, "        if: false\n")), "Full jstests")
        self.assert_rejected(self.check(self.mutate('run: scripts/ci-suite.sh jstests "$CI_LANE"', 'run: "true"')), "Full jstests")
        wf = self.workflow
        at = wf.index("      - name: Full jstests\n")
        end = wf.index("\n      - ", at + 1)
        self.assert_rejected(self.check(wf[:at] + wf[end + 1:]), "Full jstests")
        step = wf[at:end].replace("${{ runner.temp }}/testsrc", "firefox")
        self.assert_rejected(self.check(wf[:at] + step + wf[end:]), "Full jstests")

    def test_the_jit_tests_have_engine_ymls_timeout(self):
        timeout = "        timeout-minutes: 125\n        env:\n          JIT_TESTS:"
        for new in ("        env:\n          JIT_TESTS:", "        timeout-minutes: 300\n        env:\n          JIT_TESTS:"):
            with self.subTest(new=new):
                self.assert_rejected(self.check(self.mutate(timeout, new)), "jit-tests from the release test sources")

    def test_the_jit_tests_run_after_a_failed_regression_step(self):
        jit_if = ("        if: ${{ !cancelled() && steps.build.outcome == 'success' && inputs.jit_tests != 'sharded'"
                  " && inputs.jit_tests != 'none' }}\n        timeout-minutes: 125\n")
        self.assert_rejected(self.check(self.mutate(jit_if, "        timeout-minutes: 125\n")),
                             "jit-tests from the release test sources")

    def test_the_build_uses_the_release_paths(self):
        self.assert_rejected(
            self.check(self.mutate('-DSPIDERMONKEY_DIST="$RUNNER_TEMP/spidermonkey"',
                                   '-DSPIDERMONKEY_DIST="$GITHUB_WORKSPACE/firefox/obj-nightmonkey-sm/dist"')),
            "Build NightMonkey against the release")
        self.assert_rejected(self.check(self.mutate(' -DNIGHT_TOOLCHAIN_ROOT="$RUNNER_TEMP/toolchain/clang"', "")),
                             "Build NightMonkey against the release")

    def test_the_lanes_use_the_release_test_sources(self):
        # Any other step that runs jit-tests or jstests takes them from the
        # release too.
        for cmd in ('scripts/ci-suite.sh jstests "$CI_LANE" -- non262/', 'scripts/ci-suite.sh jit "$CI_LANE" -- warp/'):
            with self.subTest(cmd=cmd):
                wf = self.mutate("      - name: Upload execution logs\n",
                                 f"      - name: More tests\n        run: {cmd}\n      - name: Upload execution logs\n")
                self.assert_rejected(self.check(wf), "runs jit-tests or jstests without")
                # With the test root it is still refused: a step that mentions
                # the release directories must be one of the allowlisted steps.
                wf = self.mutate("      - name: Upload execution logs\n",
                                 f"      - name: More tests\n        env:\n          NIGHT_FIREFOX_TESTS: ${{{{ runner.temp }}}}/testsrc\n"
                                 f"        run: {cmd}\n      - name: Upload execution logs\n")
                self.assert_rejected(self.check(wf), "More tests")

    def test_the_fetch_precedes_the_build(self):
        wf = self.workflow
        fetch_start = wf.index("      - name: Fetch the engine release\n")
        fetch_end = wf.index("\n      - ", fetch_start + 1) + 1
        fetch = wf[fetch_start:fetch_end]
        without = wf[:fetch_start] + wf[fetch_end:]
        build_at = without.index("      - name: Build NightMonkey against the release\n")
        moved = without[:build_at] + without[build_at:]
        nxt = moved.index("\n      - ", build_at + 1) + 1
        moved = moved[:nxt] + fetch + moved[nxt:]
        self.assert_rejected(self.check(moved), "order")

    def test_no_source_build_or_other_download_in_this_workflow(self):
        for line in ("run: engine-src/scripts/build-engine.sh", "run: cat engine/QUEUE_PIN",
                     "run: gh release download latest -R YurtOS/yurt-firefox",
                     "run: cat engine/ENGINE_RELEASE", "run: ./mach build"):
            with self.subTest(line=line):
                wf = self.mutate("      - name: Prepare logs\n", f"      - name: Sneak\n        {line}\n      - name: Prepare logs\n")
                self.assert_rejected(self.check(wf), "Sneak")

    def sneak(self, lines, name="Sneak"):
        body = "".join(f"        {l}\n" for l in lines)
        return self.mutate("      - name: Prepare logs\n", f"      - name: {name}\n{body}      - name: Prepare logs\n")

    PROBES = (
        # reviewers' probes (round 3)
        ['run: cp -r "cache #1/." "$RUNNER_TEMP/testsrc"'],
        ['run: echo "# x"; cp -r cache/. "$RUNNER_TEMP/testsrc"'],
        ["env:", "  D: ${{ runner.temp }}/spidermonkey", 'run: cp -r out "$D"'],
        ['run: d="$RUNNER_TEMP/spidermonkey"; cp -r out "$d"'],
        ["run: |", "  cp -r out \\", '    "$RUNNER_TEMP/toolchain"'],
        ['run: echo "a # b" > "$RUNNER_TEMP/testsrc/x"'],
        ['run: tar --extract -f x.tar -C "$RUNNER_TEMP/testsrc"'],
        ['run: zstd -d -o "$RUNNER_TEMP/spidermonkey/lib/libjs_static.a" x.zst'],
        # earlier probes
        ['run: cp -r src "$RUNNER_TEMP"/testsrc'],
        ['run: cp -r src "$RUNNER_TEMP/testsrc"'],
        ["run: cp -r src ${RUNNER_TEMP}/spidermonkey"],
        ["run: cp -r src ${{ runner.temp }}//toolchain"],
        ['run: echo x > "$RUNNER_TEMP/toolchain/f"'],
        ['run: zstd -dc x.tar.zst | tar -x -C "$RUNNER_TEMP/testsrc"'],
        ['run: curl -fsSL https://example.invalid/e.tar | tar -x -C "$RUNNER_TEMP"'],
        ['run: curl -fsSL -o "$RUNNER_TEMP/e.tar.zst" https://example.invalid/e'],
        ["run: wget -q https://example.invalid/e.tar"],
        ["run: git clone https://github.com/YurtOS/yurt-firefox engine"],
        ["run: git -C engine fetch origin main"],
        ["run: gh release download engine-v1-1 -R YurtOS/yurt-firefox"],
        ["run: gh api repos/YurtOS/yurt-firefox/releases"],
        ["uses: actions/download-artifact@9000827ccba6bdab643e8b6fd33ac0654aef8333 # v8.0.2",
         "with:", "  name: x", "  path: elsewhere"],
        ["uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1", "with:", "  repository: YurtOS/yurt-firefox"],
        ["uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1", "with:", '  repository: "YurtOS/yurt-firefox"'],
        ["uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1", "with:", "  repository: 'yurtos/yurt-firefox'"],
        ["uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1", "with:", "  repository: some/other-repo"],
        ["uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1", "with:", '  "repository": YurtOS/yurt-firefox'],
        # Anywhere in the step, any spelling: flow mapping, quotes, case, spacing.
        ["uses: %s" % CHECKOUT, "with: {repository: YurtOS/yurt-firefox, path: engine}"],
        ["uses: %s" % CHECKOUT, "with:", "  'Repository': yurtos/yurt-firefox"],
        ["uses: %s" % CHECKOUT, "with:", '  REPOSITORY: "YurtOS/Yurt-Firefox"'],
        ["uses: %s" % CHECKOUT, "with:", "  repository : YurtOS/yurt-firefox"],
        ["uses: %s" % CHECKOUT, 'with: {"repository": "YurtOS/yurt-firefox"}'],
        ["run: CURL -fsSL https://example.invalid/e.tar"],
        ['run: ls "$RUNNER_TEMP/SpiderMonkey"'],
    )

    def test_every_probe_is_refused(self):
        for lines in self.PROBES:
            with self.subTest(probe=lines):
                self.assert_rejected(self.check(self.sneak(lines)), "Sneak")

    def test_reads_and_comments_are_over_flagged_by_design(self):
        """Only the allowlisted steps may mention the release directories, a
        read included, and a mention in a comment counts: a new step that
        needs them is added to the allowlist deliberately."""
        for lines in (['run: ls "$RUNNER_TEMP/testsrc"'], ["run: echo ok # curl is not needed"],
                      ["env:", "  NIGHT_FIREFOX_TESTS: ${{ runner.temp }}/testsrc", 'run: ls "$NIGHT_FIREFOX_TESTS"']):
            with self.subTest(lines=lines):
                self.assert_rejected(self.check(self.sneak(lines, name="Harmless")), "Harmless")

    def test_the_packaging_steps_are_exact(self):
        for name, old, new in (("Package the shard inputs", '-rf "$RUNNER_TEMP/shard-inputs.tar" js\n',
                                '-rf "$RUNNER_TEMP/shard-inputs.tar" js && cp -r x "$RUNNER_TEMP/testsrc"\n'),
                               ("Package the jstests inputs", "js xul-info.txt\n", "js xul-info.txt ../spidermonkey\n")):
            with self.subTest(step=name):
                self.assert_rejected(self.check(self.mutate(old, new)), name)

    def test_other_jobs_are_not_the_engine_job(self):
        docs = ("\n  docs:\n    runs-on: ubuntu-24.04\n    steps:\n"
                "      - name: Fetch the docs\n        run: curl -fsSL https://example.invalid/docs\n")
        r = self.check(self.workflow + docs)
        self.assertEqual(r.returncode, 0, r.stderr)
        # The same download in the engine job still fails.
        self.assert_rejected(self.check(self.sneak(["run: curl -fsSL https://example.invalid/docs"])), "Sneak")

    def test_job_level_if_and_unpinned_actions_are_rejected(self):
        self.assert_rejected(self.check(self.mutate("    runs-on: ubuntu-24.04\n", "    if: ${{ inputs.lane != 'x' }}\n    runs-on: ubuntu-24.04\n")),
                             "job-level if")
        self.assert_rejected(self.check(self.mutate(
            "uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1",
            "uses: actions/checkout@v7")), "pinned")

    def test_comment_lines_are_ignored(self):
        wf = self.mutate("      - name: Prepare logs\n", "      # gh release download latest; engine/ENGINE_RELEASE\n      - name: Prepare logs\n")
        self.assertEqual(self.check(wf).returncode, 0)


def step_text(workflow, name):
    lines = workflow.splitlines()
    start = next(i for i, l in enumerate(lines) if l.strip() == f"- name: {name}")
    out = [lines[start]]
    for l in lines[start + 1:]:
        if l.strip() and len(l) - len(l.lstrip()) <= len(lines[start]) - len(lines[start].lstrip()):
            break
        out.append(l)
    return "\n".join(out).rstrip()


class RegressionStepsTests(unittest.TestCase):
    """The release job is the only engine job: it runs the regression steps
    the source build ran (engine.yml, removed), on the build it makes."""

    def test_the_regression_steps(self):
        release = (REPO / ".github/workflows/engine-release.yml").read_text()
        for name, command in (("Guest tier options contract", "run: python3 scripts/test-guest-tier-options.py build"),
                              ("Local regressions", 'run: scripts/ci-suite.sh local "$CI_LANE"'),
                              ("Tier regressions (baseline, MIR stress)", "scripts/run-night-tests.sh build")):
            with self.subTest(step=name):
                text = step_text(release, name)
                self.assertIn("if: ${{ !cancelled() && steps.build.outcome == 'success' }}", text)
                self.assertIn(command, text)


def ci_job(ci, name):
    import re
    m = re.search(rf"^  {name}:\n((?:    .*\n|\n)*)", ci + "\n", re.M)
    if m is None:
        raise AssertionError(f"ci.yml has no {name} job")
    return m.group(1)


class CallersTests(unittest.TestCase):
    """The engine comes only from the release: every caller uses
    engine-release.yml, and the source build is gone."""

    WORKFLOWS = REPO / ".github/workflows"

    def test_ci_has_one_engine_job_on_the_release(self):
        import re
        ci = (self.WORKFLOWS / "ci.yml").read_text()
        job = ci_job(ci, "engine")
        self.assertIn("    needs: tier\n", job)
        self.assertIn("    uses: ./.github/workflows/engine-release.yml\n", job)
        self.assertIn("      jit_tests: ${{ needs.tier.outputs.jit_tests }}\n", job)
        self.assertEqual(re.findall(r"^    if: (.*)$", job, re.M), ["needs.tier.outputs.docs_only != 'true'"])
        self.assertNotIn("\n  engine-release:\n", ci)
        self.assertIn("    needs: [tier, engine]\n", ci_job(ci, "jit-shards"))

    def test_ci_checks_the_release_pin_only(self):
        ci = (self.WORKFLOWS / "ci.yml").read_text()
        self.assertIn("run: scripts/check-engine-release.sh\n", ci)
        self.assertNotIn("check-engine-source", ci)

    def test_the_jstests_workflows_use_the_release(self):
        nightly = (self.WORKFLOWS / "jstests.yml").read_text()
        self.assertIn("    uses: ./.github/workflows/engine-release.yml\n", nightly)
        self.assertIn("      full_jstests: true\n", nightly)
        weekly = (self.WORKFLOWS / "jstests-aot.yml").read_text()
        self.assertIn("    uses: ./.github/workflows/engine-release.yml\n", weekly)
        self.assertIn("      jstests_inputs: true\n", weekly)

    def test_no_workflow_calls_the_source_build(self):
        for wf in sorted(self.WORKFLOWS.glob("*.yml")):
            with self.subTest(workflow=wf.name):
                text = "\n".join(l for l in wf.read_text().splitlines() if not l.lstrip().startswith("#"))
                self.assertNotIn("engine.yml", text.replace("engine-release.yml", ""))
                for word in ("QUEUE_PIN", "check-engine-source", "build-engine.sh", "engine-src", "SM_OBJDIR"):
                    self.assertNotIn(word, text)

    def test_the_source_build_files_are_gone(self):
        for path in (".github/workflows/engine.yml", "engine/QUEUE_PIN", "scripts/check-engine-source.sh",
                     "scripts/tests/test_check_engine_source.py", "spidermonkey/mozconfig",
                     "scripts/build-spidermonkey.sh"):
            with self.subTest(path=path):
                self.assertFalse((REPO / path).exists(), path)


if __name__ == "__main__":
    unittest.main()
