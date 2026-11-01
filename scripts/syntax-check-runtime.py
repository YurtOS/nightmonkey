#!/usr/bin/env python3
"""Compile the C++ runtime and shells with -fsyntax-only from a configured
CMake build's compile_commands.json, without building anything.

    scripts/syntax-check-runtime.py <build-dir> [--include-dir DIR]... [--jobs N]

The build directory is a CMake configure (CMAKE_EXPORT_COMPILE_COMMANDS=ON)
against an engine dist. Each --include-dir is put ahead of the dist's
include directories (used to overlay a different engine's headers on a dist
whose generated headers are reused). Prints each failing translation unit's
first errors; exits non-zero if any fails.
"""

import argparse
import concurrent.futures
import json
import shlex
import subprocess
import sys
from pathlib import Path


def command_for(entry, overlay):
    args = shlex.split(entry["command"])
    out = []
    skip = False
    for a in args:
        if skip:
            skip = False
            continue
        if a == "-o":
            skip = True
            continue
        if a == "-c":
            continue
        out.append(a)
    out = out[:1] + [f"-I{d}" for d in overlay] + out[1:]
    return out + ["-fsyntax-only", "-ferror-limit=50"]


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("build", type=Path)
    parser.add_argument("--include-dir", action="append", default=[])
    parser.add_argument("--jobs", type=int, default=8)
    parser.add_argument("--filter", default="", help="only files whose path contains this")
    args = parser.parse_args()
    entries = json.loads((args.build / "compile_commands.json").read_text())
    entries = [e for e in entries if args.filter in e["file"]]
    if not entries:
        raise SystemExit("no compile commands")

    def run(entry):
        r = subprocess.run(command_for(entry, args.include_dir), cwd=entry["directory"],
                           capture_output=True, text=True)
        return entry["file"], r.returncode, r.stderr

    failed = 0
    with concurrent.futures.ThreadPoolExecutor(args.jobs) as pool:
        for path, status, err in pool.map(run, entries):
            name = Path(path).name
            if status == 0:
                print(f"ok    {name}")
            else:
                failed += 1
                errors = [l for l in err.splitlines() if "error:" in l]
                print(f"FAIL  {name}: {len(errors)} errors")
                for l in errors:
                    print(f"      {l}")
    print(f"{len(entries) - failed}/{len(entries)} translation units compile")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
