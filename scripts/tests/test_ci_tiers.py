"""Static checks of the CI tier wiring (ci.yml, engine.yml, jit-shards.yml)."""

import re
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
        self.upload = step(body("engine.yml"), "Upload the shard inputs")
        self.download = step(body("jit-shards.yml"), "Download the shard inputs")

    def test_upload_and_download_name_the_same_artifact(self):
        self.assertEqual(value(self.upload, "name"), value(self.download, "name"))

    def test_the_name_does_not_depend_on_the_attempt(self):
        for text in (self.upload, self.download):
            self.assertNotIn("run_attempt", value(text, "name"))

    def test_a_rerun_build_replaces_the_inputs(self):
        self.assertEqual(value(self.upload, "overwrite"), "true")


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


if __name__ == "__main__":
    unittest.main()
