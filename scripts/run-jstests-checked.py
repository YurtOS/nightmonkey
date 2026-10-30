"""Expose upstream completion and execution counts to our log validator,
and make --total-chunks slice one list on every runner."""
import runpy
import sys
from pathlib import Path

harness = Path(sys.argv.pop(1)).resolve()
sys.path.insert(0, str(harness.parent))
from lib.results import ResultsSink
import lib.manifest as manifest

# jstests.py --this-chunk/--total-chunks slices the tests in the order
# os.walk yields them, which depends on the file system, so shards on
# different runners need not partition the suite. Count and load from a
# sorted walk instead.
walk = manifest._find_all_js_files


def sorted_walk(location):
    yield from sorted(walk(location))


manifest._find_all_js_files = sorted_walk

finish = ResultsSink.finish


def checked_finish(self, completed):
    finish(self, completed)
    counts = self.counts
    print(f"NIGHT-JSTESTS completed={int(completed)} passed={counts['PASS']} "
          f"failed={counts['FAIL']} timeout={counts['TIMEOUT']} skipped={counts['SKIP']}")


ResultsSink.finish = checked_finish
sys.argv[0] = str(harness)
runpy.run_path(str(harness), run_name="__main__")
