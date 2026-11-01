#!/usr/bin/env python3
"""Make an engine dist with another engine version's headers, for
scripts/syntax-check-runtime.py when no build of that engine is at hand.

    scripts/overlay-engine-headers.py <base-dist> <firefox-git-dir> <ref> <Opcodes.h> <out-dist>

Copies <base-dist> (an engine release's spidermonkey/ directory: its
generated headers and js-build-config.json) to <out-dist> and puts over it
the headers of <ref> in the Firefox repository <firefox-git-dir> (the
patched tree: js/public -> include/js, mfbt -> include/mozilla, js/src ->
include-private), deletes the base's headers that <ref> no longer has
(other than generated ones), installs <Opcodes.h> (the patched engine's,
as the opcode table check reads it) and regenerates js/PrefsGenerated.h
from <ref>'s StaticPrefList.yaml (needs PyYAML).

The result is a hybrid: every generated header other than
PrefsGenerated.h (js-confdefs.h, CacheIROpsGenerated.h, selfhosted.out.h,
...) is still the base engine's. A clean syntax check against it is
strong evidence, not proof, that the runtime builds against <ref>.
"""

import io
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import types
from pathlib import Path

# Headers the base dist has that no source tree has: build outputs.
GENERATED = re.compile(r"(Generated\.h|\.out\.h|^js-confdefs\.h|^js-config\.h|^gcc_hidden\.h)$")


def export(git_dir, ref, paths, into):
    archive = subprocess.run(["git", "-C", str(git_dir), "archive", ref, *paths],
                             capture_output=True, check=True).stdout
    with tarfile.open(fileobj=io.BytesIO(archive)) as tar:
        tar.extractall(into, filter="data")


def put(dst, src):
    dst.parent.mkdir(parents=True, exist_ok=True)
    if dst.is_symlink() or dst.exists():
        dst.unlink()
    shutil.copyfile(src, dst)


def prefs_header(src, git_dir, ref, defines):
    """js/PrefsGenerated.h, by <ref>'s own GeneratePrefs.py, with the build
    system's preprocessor and configuration stubbed by the base's defines."""
    try:
        import yaml  # noqa: F401  (GeneratePrefs.py imports it)
    except ImportError:
        raise SystemExit("PyYAML is required to regenerate js/PrefsGenerated.h")
    with tempfile.TemporaryDirectory() as tmp:
        tmp = Path(tmp)
        export(git_dir, ref, ["python/mozbuild/mozbuild/preprocessor.py",
                              "modules/libpref/init/StaticPrefList.yaml"], tmp)
        mozbuild = types.ModuleType("mozbuild")
        sys.modules["mozbuild"] = mozbuild
        sys.modules["mozbuild.makeutil"] = types.SimpleNamespace(Makefile=object)
        sys.modules["mozpack"] = types.ModuleType("mozpack")
        sys.modules["mozpack.path"] = types.SimpleNamespace(
            normsep=lambda p: p.replace("\\", "/"))
        sys.modules["buildconfig"] = types.SimpleNamespace(
            defines={"ALLDEFINES": defines}, substs={})
        pre = types.ModuleType("mozbuild.preprocessor")
        pre.__file__ = str(tmp / "python/mozbuild/mozbuild/preprocessor.py")
        exec(compile(Path(pre.__file__).read_text(), pre.__file__, "exec"), pre.__dict__)
        sys.modules["mozbuild.preprocessor"] = pre
        mozbuild.preprocessor = pre
        gen = types.ModuleType("GeneratePrefs")
        path = src / "js/src/GeneratePrefs.py"
        exec(compile(path.read_text(), str(path), "exec"), gen.__dict__)
        out = io.StringIO()
        gen.generate_prefs_header(out, str(tmp / "modules/libpref/init/StaticPrefList.yaml"))
        return out.getvalue()


def main():
    if len(sys.argv) != 6:
        raise SystemExit(__doc__)
    base, git_dir, ref, opcodes_h, out = sys.argv[1:]
    base, git_dir, opcodes_h, out = map(lambda p: Path(p).resolve(),
                                        (base, git_dir, opcodes_h, out))
    if out.exists():
        shutil.rmtree(out)
    shutil.copytree(base, out, symlinks=True, ignore=shutil.ignore_patterns("lib", "bin"))
    with tempfile.TemporaryDirectory() as tmp:
        src = Path(tmp)
        export(git_dir, ref, ["js/public", "js/src", "mfbt"], src)
        for f in (src / "js/public").rglob("*.h"):
            put(out / "include/js" / f.relative_to(src / "js/public"), f)
        for f in (src / "mfbt").rglob("*.h"):
            rel = f.relative_to(src / "mfbt")
            if rel.parts[0] != "tests":
                put(out / "include/mozilla" / rel, f)
        for name in ("jsapi.h", "jsfriendapi.h", "jspubtd.h", "jstypes.h"):
            if (src / "js/src" / name).exists():
                put(out / "include" / name, src / "js/src" / name)
        skip = {"tests", "jsapi-tests", "fuzz-tests", "devtools", "octane", "build"}
        for f in (src / "js/src").rglob("*.h"):
            rel = f.relative_to(src / "js/src")
            if rel.parts[0] not in skip:
                put(out / "include-private" / rel, f)
        private = out / "include-private"
        for f in list(private.rglob("*.h")):
            rel = f.relative_to(private)
            if not (src / "js/src" / rel).exists() and not GENERATED.search(rel.name):
                f.unlink()
        # 159's ProfilingCategory.h includes it from mozilla/.
        if (out / "include/ProfilingCategoryList.h").exists():
            put(out / "include/mozilla/ProfilingCategoryList.h",
                out / "include/ProfilingCategoryList.h")
        defines = {}
        for line in (private / "js-confdefs.h").read_text().splitlines():
            m = re.match(r"#define (\w+)(?: (.*))?$", line.strip())
            if m:
                defines[m.group(1)] = m.group(2) if m.group(2) is not None else "1"
        (out / "include/js/PrefsGenerated.h").write_text(prefs_header(src, git_dir, ref, defines))
    put(private / "vm/Opcodes.h", opcodes_h)
    print(f"wrote {out}")


if __name__ == "__main__":
    main()
