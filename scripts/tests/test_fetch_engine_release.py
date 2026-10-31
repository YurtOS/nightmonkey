"""scripts/fetch-engine-release.sh and scripts/update-engine-release.sh against a
fixture release served by a stub `gh` (no network)."""

import hashlib
import io
import json
import os
import shutil
import subprocess
import tarfile
import tempfile
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
TAG = "engine-v147.0.4-1"
P = "spidermonkey_147.0.4-1"
KINDS = ("wasm32-wasi", "testsrc", "jstests", "toolchain-clang", "toolchain-sysroot")

GH_STUB = r'''#!/bin/sh
# gh release download TAG --repo R --dir D --pattern P: copy $GH_ASSETS/<TAG>/P to D/P.
echo "gh $*" >> "$GH_CALLS"
[ "$1 $2" = "release download" ] || { echo "gh stub: unexpected $*" >&2; exit 2; }
tag=$3; shift 3
dir= pat= repo=
while [ $# -gt 0 ]; do
  case $1 in
    --repo|-R) repo=$2; shift ;;
    --dir|-D) dir=$2; shift ;;
    --pattern|-p) pat=$2; shift ;;
  esac
  shift
done
[ "$repo" = YurtOS/yurt-firefox ] || { echo "gh stub: repo $repo" >&2; exit 2; }
[ -f "$GH_ASSETS/$tag/$pat" ] || { echo "no assets match the file pattern" >&2; exit 1; }
mkdir -p "$dir"
cp "$GH_ASSETS/$tag/$pat" "$dir/$pat"
'''


def tar_zst(path, members):
    """members: list of (name, kind, data/link, mode)."""
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w", format=tarfile.GNU_FORMAT) as t:
        for name, kind, data, mode in members:
            i = tarfile.TarInfo(name)
            i.mode = mode
            if kind == "dir":
                i.type = tarfile.DIRTYPE
                t.addfile(i)
            elif kind == "sym":
                i.type, i.linkname = tarfile.SYMTYPE, data
                t.addfile(i)
            else:
                i.size = len(data)
                t.addfile(i, io.BytesIO(data))
    subprocess.run(["zstd", "-q", "-f", "-o", str(path)], input=buf.getvalue(), check=True)


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def F(name, data=b"x\n", mode=0o644):
    return (name, "file", data, mode)


def D(name):
    return (name, "dir", None, 0o755)


ARCHIVES = {
    "wasm32-wasi": [D("spidermonkey"), D("spidermonkey/include-private"),
                    F("spidermonkey/include-private/js-build-config.json", b"{}\n"),
                    D("spidermonkey/lib"), F("spidermonkey/lib/libjs_static.a"), F("spidermonkey/MANIFEST.json", b"{}\n")],
    "testsrc": [D("js"), D("js/src"), D("js/src/jit-test"), F("js/src/jit-test/jit_test.py"),
                D("js/src/tests"), D("js/src/tests/lib"), F("js/src/tests/lib/__init__.py"),
                F("js/src/tests/jstests.py"), F("js/src/jsapi.h")],
    "jstests": [D("js"), D("js/src"), D("js/src/tests"), D("js/src/tests/non262"), F("js/src/tests/non262/a.js"),
                F("js/src/tests/shell.js"), F("xul-info.txt", b"wasm32-clang:WASI:false\n")],
    "toolchain-clang": [D("clang"), D("clang/bin"), F("clang/bin/clang", b"#!/bin/sh\n", 0o755),
                        ("clang/bin/clang++", "sym", "clang", 0o777)],
    "toolchain-sysroot": [D("sysroot-wasm32-wasi"), D("sysroot-wasm32-wasi/include"),
                          F("sysroot-wasm32-wasi/include/stdio.h")],
}


class ReleaseFixture(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="engine release ")
        self.addCleanup(self.temp.cleanup)
        self.t = Path(self.temp.name)
        self.repo = self.t / "repo"
        (self.repo / "scripts").mkdir(parents=True)
        (self.repo / "engine").mkdir()
        for s in ("fetch-engine-release.sh", "update-engine-release.sh"):
            shutil.copyfile(REPO / "scripts" / s, self.repo / "scripts" / s)
        self.stubs = self.t / "stubs"
        self.stubs.mkdir()
        (self.stubs / "gh").write_text(GH_STUB)
        (self.stubs / "gh").chmod(0o755)
        self.calls = self.t / "calls"
        self.calls.write_text("")
        self.assets = self.t / "assets" / TAG
        self.assets.mkdir(parents=True)
        self.make_release(ARCHIVES)
        self.dest = self.t / "runner"
        (self.dest / "nightmonkey-logs").mkdir(parents=True)
        self.dest = self.dest.resolve()

    def make_release(self, archives, tag=TAG):
        d = self.t / "assets" / tag
        d.mkdir(parents=True, exist_ok=True)
        assets = []
        for kind in KINDS:
            name = f"{P}-{kind}.tar.zst"
            tar_zst(d / name, archives[kind])
            (d / (name + ".sha256")).write_text(f"{sha(d / name)}  {name}\n")
            a = {"name": name, "sha256": sha(d / name), "size": (d / name).stat().st_size}
            assets.append(a)
        m = {"package_version": "147.0.4-1", "release": {"tag": tag, "assets": assets}}
        (d / "MANIFEST.json").write_text(json.dumps(m, indent=2, sort_keys=True) + "\n")

    def release_text(self, tag=TAG, **override):
        d = self.t / "assets" / TAG
        digests = {"manifest": sha(d / "MANIFEST.json")}
        for kind in KINDS:
            digests[kind] = sha(d / f"{P}-{kind}.tar.zst")
        digests.update(override)
        return "# fixture\n" + f"tag={tag}\n" + "".join(f"{k}={v}\n" for k, v in digests.items())

    def write_release(self, text=None):
        (self.repo / "engine/ENGINE_RELEASE").write_text(self.release_text() if text is None else text)

    def env(self):
        return {**os.environ, "PATH": f"{self.stubs}:{os.environ['PATH']}", "GH_CALLS": str(self.calls),
                "GH_ASSETS": str(self.t / "assets"), "GITHUB_OUTPUT": str(self.t / "out")}

    def fetch(self, jstests="false", *extra):
        (self.t / "out").write_text("")
        return subprocess.run(["bash", str(self.repo / "scripts/fetch-engine-release.sh"), *extra,
                               "--jstests", jstests, str(self.dest)],
                              capture_output=True, text=True, check=False, env=self.env(), cwd=self.t)

    def downloads(self):
        return [l.split("--pattern ")[1] for l in self.calls.read_text().splitlines() if "--pattern " in l]

    def extracted(self):
        return [p for p in ("spidermonkey", "toolchain", "testsrc") if (self.dest / p).exists()]


class FetchTests(ReleaseFixture):
    def test_fetch_verifies_and_extracts(self):
        self.write_release()
        r = self.fetch()
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(sorted(self.downloads()),
                         sorted(["MANIFEST.json"] + [f"{P}-{k}.tar.zst" for k in KINDS if k != "jstests"]))
        self.assertTrue((self.dest / "spidermonkey/include-private/js-build-config.json").is_file())
        self.assertTrue(os.access(self.dest / "toolchain/clang/bin/clang", os.X_OK))
        self.assertTrue(os.access(self.dest / "toolchain/clang/bin/clang++", os.X_OK))
        self.assertTrue((self.dest / "toolchain/sysroot-wasm32-wasi/include/stdio.h").is_file())
        self.assertTrue((self.dest / "testsrc/js/src/jit-test/jit_test.py").is_file())
        self.assertFalse((self.dest / "testsrc/xul-info.txt").exists())
        out = (self.t / "out").read_text()
        for line in (f"spidermonkey_dist={self.dest}/spidermonkey", f"toolchain_root={self.dest}/toolchain/clang",
                     f"sysroot={self.dest}/toolchain/sysroot-wasm32-wasi", f"testsrc={self.dest}/testsrc",
                     f"tag={TAG}"):
            self.assertIn(line + "\n", out)

    def test_full_jstests_adds_the_jstests_asset_to_the_test_sources(self):
        self.write_release()
        r = self.fetch("true")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn(f"{P}-jstests.tar.zst", self.downloads())
        self.assertTrue((self.dest / "testsrc/js/src/jit-test/jit_test.py").is_file())
        self.assertTrue((self.dest / "testsrc/js/src/tests/non262/a.js").is_file())
        self.assertEqual((self.dest / "testsrc/xul-info.txt").read_text(), "wasm32-clang:WASI:false\n")

    def test_a_changed_asset_fails_before_anything_is_extracted(self):
        for kind in KINDS:
            with self.subTest(kind=kind):
                self.setUp()
                self.write_release()
                with open(self.assets / f"{P}-{kind}.tar.zst", "ab") as f:
                    f.write(b"x")
                r = self.fetch("true")
                self.assertNotEqual(r.returncode, 0)
                self.assertIn(f"{P}-{kind}.tar.zst", r.stderr)
                self.assertEqual(self.extracted(), [])

    def test_a_changed_jstests_asset_only_matters_when_asked(self):
        self.write_release()
        with open(self.assets / f"{P}-jstests.tar.zst", "ab") as f:
            f.write(b"x")
        self.assertEqual(self.fetch("false").returncode, 0)

    def test_the_manifest_is_verified_and_must_agree(self):
        self.write_release()
        m = json.loads((self.assets / "MANIFEST.json").read_text())
        (self.assets / "MANIFEST.json").write_text(json.dumps(m) + "\n")
        r = self.fetch()
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("MANIFEST.json", r.stderr)
        # A manifest whose digest is pinned but whose asset list disagrees
        # with ENGINE_RELEASE, or whose tag differs.
        for edit in ("m['release']['assets'][1]['sha256'] = '0' * 64", "m['release']['tag'] = 'engine-v147.0.4-2'"):
            with self.subTest(edit=edit):
                self.setUp()
                m = json.loads((self.assets / "MANIFEST.json").read_text())
                exec(edit)
                (self.assets / "MANIFEST.json").write_text(json.dumps(m) + "\n")
                self.write_release()
                r = self.fetch()
                self.assertNotEqual(r.returncode, 0)
                self.assertIn("MANIFEST.json", r.stderr)
                self.assertEqual(self.extracted(), [])

    def test_the_release_file_is_checked(self):
        for text in (self.release_text(tag="PENDING"), self.release_text(tag="latest"),
                     self.release_text(testsrc="PENDING"), self.release_text().replace("testsrc=", "#testsrc=")):
            with self.subTest(text=text):
                self.calls.write_text("")
                self.write_release(text)
                r = self.fetch()
                self.assertNotEqual(r.returncode, 0)
                self.assertIn("ENGINE_RELEASE", r.stderr)
                self.assertEqual(self.downloads(), [])

    def test_a_missing_asset_fails(self):
        self.write_release()
        (self.assets / f"{P}-testsrc.tar.zst").unlink()
        r = self.fetch()
        self.assertNotEqual(r.returncode, 0)
        self.assertIn(f"{P}-testsrc.tar.zst", r.stderr)
        self.assertEqual(self.extracted(), [])

    def test_toolchain_layout_is_checked(self):
        bad = {
            "two tops": [D("clang"), D("clang/bin"), F("clang/bin/clang", b"", 0o755), F("clang/bin/clang++", b"", 0o755), D("extra")],
            "no clang++": [D("clang"), D("clang/bin"), F("clang/bin/clang", b"", 0o755)],
            "escaping link": [D("clang"), D("clang/bin"), F("clang/bin/clang", b"", 0o755),
                              ("clang/bin/clang++", "sym", "../../../usr/bin/clang", 0o777)],
            "absolute link": [D("clang"), D("clang/bin"), F("clang/bin/clang", b"", 0o755),
                              ("clang/bin/clang++", "sym", "/usr/bin/clang", 0o777)],
            "other top name": [D("llvm"), D("llvm/bin"), F("llvm/bin/clang", b"", 0o755), F("llvm/bin/clang++", b"", 0o755)],
        }
        for label, members in bad.items():
            with self.subTest(case=label):
                self.setUp()
                self.make_release(dict(ARCHIVES, **{"toolchain-clang": members}))
                self.write_release()
                r = self.fetch()
                self.assertNotEqual(r.returncode, 0, label)
                self.assertIn(f"{P}-toolchain-clang.tar.zst", r.stderr)

    def test_an_escaping_member_in_any_archive_fails(self):
        self.make_release(dict(ARCHIVES, testsrc=ARCHIVES["testsrc"] + [F("../escape")]))
        self.write_release()
        r = self.fetch()
        self.assertNotEqual(r.returncode, 0)
        self.assertIn(f"{P}-testsrc.tar.zst", r.stderr)
        self.assertFalse((self.t / "escape").exists())

    def test_existing_outputs_are_refused(self):
        self.write_release()
        (self.dest / "spidermonkey").mkdir()
        r = self.fetch()
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("spidermonkey", r.stderr)

    def test_usage(self):
        self.write_release()
        for args in (["--jstests", "yes", str(self.dest)], [str(self.dest)], ["--jstests", "true"]):
            with self.subTest(args=args):
                r = subprocess.run(["bash", str(self.repo / "scripts/fetch-engine-release.sh"), *args],
                                   capture_output=True, text=True, env=self.env(), check=False)
                self.assertEqual(r.returncode, 2, r.stderr)


class UpdateTests(ReleaseFixture):
    def update(self, *args):
        return subprocess.run(["bash", str(self.repo / "scripts/update-engine-release.sh"), *args],
                              capture_output=True, text=True, check=False, env=self.env(), cwd=self.t)

    def test_writes_the_release_file_from_the_manifest(self):
        r = self.update(TAG)
        self.assertEqual(r.returncode, 0, r.stderr)
        text = (self.repo / "engine/ENGINE_RELEASE").read_text()
        body = [l for l in text.splitlines() if l and not l.startswith("#")]
        self.assertEqual(body, [l for l in self.release_text().splitlines() if l and not l.startswith("#")])
        self.assertIn("gen_opcodes.py", r.stdout)
        # And fetch accepts what it wrote.
        self.assertEqual(self.fetch().returncode, 0)

    def test_refuses_a_manifest_that_disagrees_with_the_digest_lines(self):
        (self.assets / f"{P}-testsrc.tar.zst.sha256").write_text("0" * 64 + f"  {P}-testsrc.tar.zst\n")
        r = self.update(TAG)
        self.assertNotEqual(r.returncode, 0)
        self.assertIn(f"{P}-testsrc.tar.zst", r.stderr)
        self.assertFalse((self.repo / "engine/ENGINE_RELEASE").exists())

    def test_refuses_a_manifest_for_another_tag_or_bad_tag(self):
        m = json.loads((self.assets / "MANIFEST.json").read_text())
        m["release"]["tag"] = "engine-v147.0.4-2"
        (self.assets / "MANIFEST.json").write_text(json.dumps(m) + "\n")
        self.assertNotEqual(self.update(TAG).returncode, 0)
        for tag in ("latest", "engine-v147.0.4", ""):
            with self.subTest(tag=tag):
                self.assertEqual(self.update(tag).returncode, 2)
        self.assertFalse((self.repo / "engine/ENGINE_RELEASE").exists())

    def test_refuses_a_manifest_missing_an_asset(self):
        m = json.loads((self.assets / "MANIFEST.json").read_text())
        m["release"]["assets"] = [a for a in m["release"]["assets"] if "jstests" not in a["name"]]
        (self.assets / "MANIFEST.json").write_text(json.dumps(m) + "\n")
        r = self.update(TAG)
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("jstests", r.stderr)


if __name__ == "__main__":
    unittest.main()
