"""Expose upstream completion and execution counts to our log validator."""
import runpy
import sys
from pathlib import Path

harness = Path(sys.argv.pop(1)).resolve()
sys.path.insert(0, str(harness.parent))
from lib.results import ResultsSink

finish = ResultsSink.finish


def checked_finish(self, completed):
    finish(self, completed)
    counts = self.counts
    print(f"NIGHT-JSTESTS completed={int(completed)} passed={counts['PASS']} "
          f"failed={counts['FAIL']} timeout={counts['TIMEOUT']} skipped={counts['SKIP']}")


ResultsSink.finish = checked_finish
sys.argv[0] = str(harness)
runpy.run_path(str(harness), run_name="__main__")
