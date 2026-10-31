"""Static checks of the CI tier wiring (ci.yml, engine-release.yml, jit-shards.yml)."""

import json
import os
import re
import subprocess
import tempfile
import unittest
from pathlib import Path

WORKFLOWS = Path(__file__).resolve().parents[2] / ".github/workflows"


def body(name):
    """The workflow text without full-line comments."""
    return "\n".join(l for l in (WORKFLOWS / name).read_text().splitlines()
                     if not l.lstrip().startswith("#"))


def step(text, name):
    """The lines of the step named `name`, up to the next step."""
    lines = text.splitlines()
    start = next(i for i, l in enumerate(lines) if l.strip() == f"- name: {name}")
    indent = len(lines[start]) - len(lines[start].lstrip())
    out = [lines[start]]
    for l in lines[start + 1:]:
        if l.strip() and len(l) - len(l.lstrip()) <= indent:
            break
        out.append(l)
    return "\n".join(out)


def value(step_text, key):
    m = re.search(rf"^\s+{key}: (.*)$", step_text, re.M)
    return m.group(1).strip() if m else None


class ShardInputsTests(unittest.TestCase):
    """"Re-run failed jobs" reruns a failed shard but not the build job that
    uploaded its inputs, so the artifact name must not depend on the attempt."""

    def setUp(self):
        self.upload = step(body("engine-release.yml"), "Upload the shard inputs")
        self.package = step(body("engine-release.yml"), "Package the shard inputs")
        self.download = step(body("jit-shards.yml"), "Download the shard inputs")

    def test_upload_and_download_name_the_same_artifact(self):
        self.assertEqual(value(self.upload, "name"), value(self.download, "name"))

    def test_the_name_does_not_depend_on_the_attempt(self):
        for text in (self.upload, self.download):
            self.assertNotIn("run_attempt", value(text, "name"))

    def test_a_rerun_build_replaces_the_inputs(self):
        self.assertEqual(value(self.upload, "overwrite"), "true")

    def test_the_inputs_are_uploaded_for_the_sharded_tier_only_and_kept_a_day(self):
        self.assertEqual(value(self.upload, "if"),
                         "${{ !cancelled() && steps.build.outcome == 'success' && inputs.jit_tests == 'sharded' }}")
        self.assertEqual(value(self.package, "if"), value(self.upload, "if"))
        self.assertEqual(value(self.upload, "retention-days"), "1")

    def test_the_shards_run_the_release_test_sources_they_unpack(self):
        shards = body("jit-shards.yml")
        unpack = step(shards, "Unpack the shard inputs")
        self.assertIn('tar -x -C "$RUNNER_TEMP/testsrc"', unpack)
        run = step(shards, "jit-tests (shard ${{ matrix.shard }})")
        self.assertIn('NIGHT_FIREFOX_TESTS="$RUNNER_TEMP/testsrc" \\', run)

    def test_the_inputs_are_the_shell_and_the_release_test_sources(self):
        self.assertIn("inputs.jit_tests == 'sharded'", self.package)
        self.assertIn('tar -C "$GITHUB_WORKSPACE" -cf "$RUNNER_TEMP/shard-inputs.tar" build/bin', self.package)
        self.assertIn('tar -C "$RUNNER_TEMP/testsrc" -rf "$RUNNER_TEMP/shard-inputs.tar" js', self.package)


class OtherLabelTests(unittest.TestCase):
    """A run started by a label other than ci:full gets a group of its own
    (it must not cancel the PR's run) and skips every job."""

    def setUp(self):
        self.ci = body("ci.yml")

    def test_other_label_runs_get_their_own_group(self):
        group = re.search(r"^concurrency:\n  group: (.*)$", self.ci, re.M).group(1)
        self.assertIn("github.event.action == 'labeled' && github.event.label.name != 'ci:full'", group)
        self.assertIn("github.run_id", group)

    def test_the_tier_job_skips_other_label_runs(self):
        self.assertIn("    if: github.event.action != 'labeled' || github.event.label.name == 'ci:full'",
                      self.ci)

    def test_every_other_job_needs_the_tier(self):
        section = self.ci[self.ci.index("\njobs:\n"):] + "\n"
        jobs = re.findall(r"^  ([\w-]+):\n((?:    .*\n|\n)*)", section, re.M)
        names = [name for name, _ in jobs]
        self.assertIn("tier", names)
        for name, text in jobs:
            if name == "tier":
                continue
            with self.subTest(job=name):
                self.assertRegex(text, re.compile(r"^    needs: (tier|\[tier.*\])$", re.M))


def job(text, name):
    """The lines of the top-level job `name`, up to the next job."""
    m = re.search(rf"^  {name}:\n((?:    .*\n|\n)*)", text + "\n", re.M)
    return m.group(1)


def run_block(step_text):
    """The shell of a step's `run: |` block, dedented."""
    lines = step_text.splitlines()
    start = next(i for i, l in enumerate(lines) if l.strip() == "run: |")
    out = []
    for l in lines[start + 1:]:
        out.append(l[10:] if l.strip() else "")
    return "\n".join(out) + "\n"


class DocsOnlyTests(unittest.TestCase):
    """A pull request that changes only Markdown runs only the lint jobs:
    Rust (fmt) and workflows. The engine, the scripts job and the other
    Rust checks skip. Every other event runs everything."""

    def setUp(self):
        self.ci = body("ci.yml")
        self.tier = step(self.ci, "Pick the jit-test tier")

    def pick(self, event="pull_request", files=None, labels="", fail=None):
        """Run the tier step with a fake gh; return (exit code, outputs)."""
        with tempfile.TemporaryDirectory() as d:
            gh = Path(d, "gh")
            files_text = "\n".join(files or []) + ("\n" if files else "")
            Path(d, "files").write_text(files_text)
            Path(d, "labels").write_text(labels)
            gh.write_text(
                "#!/usr/bin/env bash\n"
                f"case \"$*\" in *{fail or 'NO-FAILURE'}*) exit 1 ;; esac\n"
                f"case \"$*\" in *pulls/*/files*) cat {d}/files ;; *labels*) cat {d}/labels ;; *) exit 9 ;; esac\n")
            gh.chmod(0o755)
            out = Path(d, "out")
            out.write_text("")
            env = dict(os.environ, PATH=f"{d}:{os.environ['PATH']}", GITHUB_EVENT_NAME=event,
                       GITHUB_REPOSITORY="o/r", PR="1", GITHUB_OUTPUT=str(out),
                       GITHUB_STEP_SUMMARY=str(Path(d, "summary")), GH_TOKEN="x")
            r = subprocess.run(["bash", "-c", run_block(self.tier)], env=env, capture_output=True, text=True)
            outputs = dict(l.split("=", 1) for l in out.read_text().splitlines() if "=" in l)
            return r.returncode, outputs

    def test_markdown_only_runs_only_fmt(self):
        code, out = self.pick(files=["README.md", "docs/plan.md"])
        self.assertEqual(code, 0)
        self.assertEqual(out["docs_only"], "true")
        self.assertEqual(json.loads(out["rust_checks"]), ["fmt"])
        self.assertEqual(out["jit_tests"], "subset")

    def test_a_code_file_runs_everything(self):
        code, out = self.pick(files=["README.md", "scripts/ci-suite.sh"])
        self.assertEqual(code, 0)
        self.assertEqual(out["docs_only"], "false")
        self.assertEqual(json.loads(out["rust_checks"]), ["fmt", "check", "test", "clippy"])

    def test_a_rename_from_code_to_markdown_runs_everything(self):
        # previous_filename is listed too, so the code path it left counts.
        code, out = self.pick(files=["notes.md", "scripts/old.sh"])
        self.assertEqual(out["docs_only"], "false")

    def test_ci_full_runs_everything(self):
        code, out = self.pick(files=["README.md"], labels="ci:full\n")
        self.assertEqual(code, 0)
        self.assertEqual(out["docs_only"], "false")
        self.assertEqual(out["jit_tests"], "sharded")

    def test_push_runs_everything_without_the_api(self):
        code, out = self.pick(event="push", fail="api")
        self.assertEqual(code, 0)
        self.assertEqual(out["docs_only"], "false")
        self.assertEqual(json.loads(out["rust_checks"]), ["fmt", "check", "test", "clippy"])

    def test_an_api_error_fails_closed(self):
        code, out = self.pick(files=["README.md"], fail="files")
        self.assertNotEqual(code, 0)
        self.assertNotEqual(out.get("docs_only"), "true")

    def test_an_empty_file_list_fails_closed(self):
        code, out = self.pick(files=[])
        self.assertNotEqual(code, 0)
        self.assertNotEqual(out.get("docs_only"), "true")

    def test_the_file_list_includes_previous_names(self):
        self.assertIn("previous_filename", self.tier)

    def test_the_tier_job_exports_the_outputs(self):
        tier = job(self.ci, "tier")
        self.assertIn("docs_only: ${{ steps.tier.outputs.docs_only }}", tier)
        self.assertIn("rust_checks: ${{ steps.tier.outputs.rust_checks }}", tier)

    def test_heavy_jobs_skip_on_docs_only(self):
        for name in ("engine", "scripts"):
            with self.subTest(job=name):
                self.assertIn("if: needs.tier.outputs.docs_only != 'true'", job(self.ci, name))

    def test_rust_matrix_comes_from_the_tier(self):
        self.assertIn("check: ${{ fromJSON(needs.tier.outputs.rust_checks) }}", job(self.ci, "rust"))

    def test_the_lint_jobs_always_run(self):
        self.assertNotIn("docs_only", job(self.ci, "workflows"))

    def test_the_classifier_test_runs_on_docs_only_pull_requests(self):
        # The scripts job skips on a Markdown-only PR, so a PR that broke the
        # classifier into answering "docs-only" would skip its own test.
        self.assertIn("run: python3 -m unittest discover -s scripts/tests -p test_ci_tiers.py",
                      job(self.ci, "workflows"))


if __name__ == "__main__":
    unittest.main()
