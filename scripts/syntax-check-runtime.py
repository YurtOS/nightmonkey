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


def include_dirs(dirs):
    """The overlay include directories, absolute (the compiles run in the
    build's directories); a missing one is an error."""
    out = []
    for d in dirs:
        path = Path(d).resolve()
        if not path.is_dir():
            raise SystemExit(f"--include-dir {d}: not a directory")
        out.append(str(path))
    return out


def command_for(entry, overlay):
    """The entry's compile as a syntax-only check: without its output and
    -c, with the overlay include directories first among the compiler's
    flags (after any launcher such as ccache)."""
    args = list(entry["arguments"]) if "arguments" in entry else shlex.split(entry["command"])
    out = []
    skip = False
    for a in args:
        if skip:
            skip = False
            continue
        if a == "-o":
            skip = True
            continue
        if a == "-c" or (a.startswith("-o") and len(a) > 2):
            continue
        out.append(a)
    first_flag = next((i for i, a in enumerate(out) if a.startswith("-")), len(out))
    out = out[:first_flag] + [f"-I{d}" for d in overlay] + out[first_flag:]
    return out + ["-fsyntax-only", "-ferror-limit=50"]


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("build", type=Path)
    parser.add_argument("--include-dir", action="append", default=[])
    parser.add_argument("--jobs", type=int, default=8)
    parser.add_argument("--filter", default="", help="only files whose path contains this")
    args = parser.parse_args()
    overlay = include_dirs(args.include_dir)
    entries = json.loads((args.build / "compile_commands.json").read_text())
    entries = [e for e in entries if args.filter in e["file"]]
    if not entries:
        raise SystemExit("no compile commands")

    def label(entry):
        # The object file names a translation unit uniquely (a source built
        # into two targets, such as the shell's, has one per target).
        if "output" in entry:
            return entry["output"]
        args = list(entry["arguments"]) if "arguments" in entry else shlex.split(entry["command"])
        for i, a in enumerate(args):
            if a == "-o" and i + 1 < len(args):
                return args[i + 1]
            if a.startswith("-o") and len(a) > 2:
                return a[2:]
        return entry["file"]

    def run(entry):
        r = subprocess.run(command_for(entry, overlay), cwd=entry["directory"],
                           capture_output=True, text=True)
        return label(entry), r.returncode, r.stderr

    failed = 0
    with concurrent.futures.ThreadPoolExecutor(args.jobs) as pool:
        for path, status, err in pool.map(run, entries):
            name = path
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
