"""Validate automation records independently of the upstream exit/summary."""

import re
import sys
from pathlib import Path


def main():
    if len(sys.argv) != 3 or sys.argv[1] not in ("jit", "jstests"):
        print("usage: check-test-log.py jit|jstests LOG", file=sys.stderr)
        return 2
    suite, path = sys.argv[1:]
    text = Path(path).read_text(encoding="utf-8", errors="replace")
    if re.search(r"^TEST-UNEXPECTED-", text, re.MULTILINE):
        print("unexpected upstream test results", file=sys.stderr)
        return 2
    passed = len(re.findall(r"^TEST-PASS(?: \(EXPECTED RANDOM\))? \|", text, re.MULTILINE))
    if not passed:
        print("no passing execution results", file=sys.stderr)
        return 2
    if suite == "jit":
        totals = re.findall(r"^Passed: ([0-9]+)$", text, re.MULTILINE)
        failed = re.findall(r"^Failed: ([0-9]+)$", text, re.MULTILINE)
        if (not re.search(r"^PASSED ALL$", text, re.MULTILINE)
                or totals != [str(passed)] or failed != ["0"]):
            print("jit-test summary is missing, incomplete, or disagrees with results", file=sys.stderr)
            return 2
    print(f"night: {suite}: {passed} passing execution results verified")
    return 0


if __name__ == "__main__":
    sys.exit(main())
