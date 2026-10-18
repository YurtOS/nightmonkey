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
    records = re.findall(r"^TEST-PASS(?: \(EXPECTED RANDOM\))? \|.*$", text, re.MULTILINE)
    skipped = sum("(code 59," in record for record in records) if suite == "jit" else 0
    passed = len(re.findall(r"^TEST-PASS(?: \(EXPECTED RANDOM\))? \|", text, re.MULTILINE))
    if passed <= skipped:
        print("no passing execution results", file=sys.stderr)
        return 2
    if suite == "jit":
        totals = re.findall(r"^Passed: ([0-9]+)$", text, re.MULTILINE)
        failed = re.findall(r"^Failed: ([0-9]+)$", text, re.MULTILINE)
        if (not re.search(r"^PASSED ALL$", text, re.MULTILINE)
                or totals != [str(passed)] or failed != ["0"]):
            print("jit-test summary is missing, incomplete, or disagrees with results", file=sys.stderr)
            return 2
    else:
        summary = re.findall(r"^NIGHT-JSTESTS completed=1 passed=([0-9]+) failed=0 timeout=0 skipped=[0-9]+$", text, re.MULTILINE)
        if summary != [str(passed)]:
            print("jstests completion is missing or disagrees with results", file=sys.stderr)
            return 2
    print(f"night: {suite}: {passed - skipped} passing execution results verified")
    return 0


if __name__ == "__main__":
    sys.exit(main())
