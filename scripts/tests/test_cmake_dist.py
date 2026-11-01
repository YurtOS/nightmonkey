"""Configure the CMake build against fixture engine dists.

A packaged engine's js-build-config.json names a bare `clang`/`clang++` and
no sysroot; the consumer provides both with NIGHT_TOOLCHAIN_ROOT and
NIGHT_SYSROOT. These tests run a configure-only against a fake dist whose
config copies the real shape and read the generated compile and link lines.

What the stub compilers must answer (spiked with CMake 4.4): nothing. The
config sets CMAKE_<LANG>_COMPILER_WORKS, but CMakeDetermineCompilerId still
runs the compiler on CMakeCCompilerId.c and CMakeCXXCompilerId.cpp, and the
ABI check compiles once. A stub that only exists and is executable (any exit
status, no output) leaves the identification "unknown" and the ABI check
"failed", and configure succeeds. A compiler path that does not exist fails
at project() ("is not a full path to an existing compiler tool"), so the
producer paths in the fixture get stubs too: a missing producer compiler
would fail configure for that reason instead of the asserted one.
"""

import json
import os
import shlex
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

SOURCE = Path(__file__).resolve().parents[2]

# A fixture engine table. The opcode check test generates a matching ff147
# table for it in a copy of the source tree (the real Opcodes.h is not
# available here).
OPCODES_H = """#define JSOP_SEMANTICS_VERSION 1
#define FOR_EACH_OPCODE(MACRO) \\
  MACRO(Undefined, undefined, NULL, 1, 0, 1, JOF_BYTE) \\
  MACRO(Pop, pop, NULL, 1, 1, 0, JOF_BYTE)
#define FOR_EACH_TRAILING_UNUSED_OPCODE(MACRO)
"""


def stub(path):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("#!/bin/sh\nexit 0\n")
    path.chmod(0o755)


def is_sysroot_option(token):
    return token in ("--sysroot", "-isysroot") or token.startswith(("--sysroot=", "-isysroot"))


class CMakeDistTests(unittest.TestCase):
    def setUp(self):
        # No spaces (the config's command strings are split on them) and
        # resolved (macOS /var is a link to /private/var).
        self.base = Path(tempfile.mkdtemp(prefix="cmakedist")).resolve()
        self.addCleanup(shutil.rmtree, self.base)
        self.producer = self.base / "producer"
        self.producer_sysroot = self.producer / "sysroot-wasm32-wasi"
        self.producer_sysroot.mkdir(parents=True)
        for name in ("clang", "clang++"):
            stub(self.producer / "clang/bin" / name)
            stub(self.base / "toolchain/bin" / name)
            # A clang on PATH, so a fallback to PATH configures on any host.
            stub(self.base / "path" / name)
        self.toolchain = self.base / "toolchain"
        self.sysroot = self.base / "wasi-sysroot"
        self.sysroot.mkdir()
        self.dist = self.base / "dist"
        (self.dist / "include-private/vm").mkdir(parents=True)
        self.opcodes_h = self.dist / "include-private/vm/Opcodes.h"
        self.opcodes_h.write_text(OPCODES_H)
        self.env = {k: v for k, v in os.environ.items()
                    if k not in ("CC", "CXX", "CMAKE_GENERATOR", "CMAKE_EXPORT_COMPILE_COMMANDS")}
        self.env["PATH"] = str(self.base / "path") + os.pathsep + os.environ["PATH"]
        self.builds = 0

    def write_config(self, sysroot_option, bare=False, bare_cc=None, launcher=None):
        """The real config's shape (macOS build, warnings trimmed). A launcher
        (an engine built with CCACHE=ccache) prefixes both compilers."""
        absolute = str(self.producer / "clang/bin/clang")
        cxx = "clang" if bare else absolute
        cc = "clang" if (bare if bare_cc is None else bare_cc) else absolute
        if launcher:
            cxx, cc = f"{launcher} {cxx}", f"{launcher} {cc}"
        flags = " ".join(sysroot_option)
        config = {
            "ar": str(self.producer / "clang/bin/llvm-ar"),
            "cc": f"{cc} {flags} --target=wasm32-wasi",
            "cxx": f"{cxx}++ {flags} -std=gnu++20 --target=wasm32-wasi",
            "cxx_base_flags": [*sysroot_option, "-std=gnu++20", "--target=wasm32-wasi"],
            "debug_defines": ["NDEBUG", "TRIMMED"],
            "debug_flags": [],
            "extra_cxxflags": ["-fno-strict-aliasing", "-ffp-contract=off"],
            "force_includes": ["gcc_hidden.h", "js-confdefs.h"],
            "include_dirs": ["include-private", "include", "system_wrappers"],
            "js_shell_wizer": False,
            "libraries": ["lib/libjsshell.a", "lib/libjs_static.a", "lib/libjsrust.a",
                          "lib/libpure_virtual.a"],
            "library_defines": ["MOZILLA_CLIENT", "EXPORT_JS_API", "MOZ_HAS_MOZGLUE",
                                "MOZ_SUPPORT_LEAKCHECKING"],
            "moz_debug": False,
            "optimize_flags": ["-O"],
            "os_arch": "WASI",
            "os_cxxflags": ["-fno-rtti", "-fno-exceptions", "-fPIC"],
            "os_ldflags": ["-fuse-ld=lld"],
            "rust_target": "wasm32-wasip1",
            "topobjdir": str(self.producer / "obj-nightmonkey-sm"),
            "topsrcdir": str(self.producer),
            "warnings_cxxflags": ["-Wall"],
        }
        (self.dist / "include-private/js-build-config.json").write_text(json.dumps(config, indent=2))

    def legacy_config(self):
        self.write_config(["--sysroot", str(self.producer_sysroot)])

    def package_config(self):
        self.write_config([], bare=True)

    def provisioned(self):
        return {"NIGHT_TOOLCHAIN_ROOT": self.toolchain, "NIGHT_SYSROOT": self.sysroot}

    def configure(self, source=SOURCE, **variables):
        self.builds += 1
        build = self.base / f"build{self.builds}"
        defines = [f"-D{k}={v}" for k, v in variables.items()]
        result = subprocess.run(
            ["cmake", "-S", str(source), "-B", str(build), "-G", "Unix Makefiles",
             f"-DSPIDERMONKEY_DIST={self.dist}", "-DCMAKE_EXPORT_COMPILE_COMMANDS=ON", *defines],
            env=self.env, capture_output=True, text=True, check=False,
        )
        return build, result

    def configured(self, source=SOURCE, **variables):
        build, result = self.configure(source, **variables)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        return build

    def compiler(self, build, lang):
        """The compiler CMake recorded for C or CXX (set before project(), so not cached)."""
        recorded = list(build.glob(f"CMakeFiles/*/CMake{lang}Compiler.cmake"))
        self.assertEqual(len(recorded), 1, recorded)
        first = recorded[0].read_text().splitlines()[0]
        prefix = f'set(CMAKE_{lang}_COMPILER "'
        self.assertTrue(first.startswith(prefix) and first.endswith('")'), first)
        return first[len(prefix):-2]

    def command_lines(self, build):
        """Every compile line and both shells' link lines, as argument lists."""
        compiles = json.loads((build / "compile_commands.json").read_text())
        lines = [shlex.split(entry["command"]) for entry in compiles]
        self.assertTrue(lines, "no compile lines")
        for shell in ("js", "js-inproc"):
            lines.append(shlex.split((build / f"CMakeFiles/{shell}.dir/link.txt").read_text()))
        return lines

    def test_legacy_absolute_config_is_used_as_is(self):
        self.legacy_config()
        build = self.configured()
        self.assertEqual(self.compiler(build, "C"), str(self.producer / "clang/bin/clang"))
        self.assertEqual(self.compiler(build, "CXX"), str(self.producer / "clang/bin/clang++"))
        expected = ["--sysroot", str(self.producer_sysroot), "-std=gnu++20", "--target=wasm32-wasi"]
        for line in self.command_lines(build):
            with self.subTest(line=line[:2]):
                self.assertEqual(line[0], str(self.producer / "clang/bin/clang++"))
                self.assertIn(expected, [line[i:i + 4] for i in range(len(line))])

    def test_compiler_launcher_in_the_config_is_dropped(self):
        # mach records CC/CXX with the ccache or sccache it built through; the
        # compiler is the clang after it.
        for launcher in ("/usr/bin/ccache", "/opt/sccache/bin/sccache"):
            with self.subTest(launcher=launcher):
                self.write_config(["--sysroot", str(self.producer_sysroot)], launcher=launcher)
                build = self.configured()
                self.assertEqual(self.compiler(build, "C"), str(self.producer / "clang/bin/clang"))
                self.assertEqual(self.compiler(build, "CXX"), str(self.producer / "clang/bin/clang++"))
                for line in self.command_lines(build):
                    self.assertEqual(line[0], str(self.producer / "clang/bin/clang++"))
                    self.assertNotIn(launcher, line)

    def test_provisioned_toolchain_and_sysroot_replace_the_config(self):
        spellings = {
            "package": None,
            "--sysroot X": ["--sysroot", str(self.producer_sysroot)],
            "--sysroot=X": ["--sysroot=" + str(self.producer_sysroot)],
            "-isysroot X": ["-isysroot", str(self.producer_sysroot)],
            "-isysrootX": ["-isysroot" + str(self.producer_sysroot)],
        }
        for spelling, option in spellings.items():
            with self.subTest(spelling=spelling):
                if option is None:
                    self.package_config()
                else:
                    self.write_config(option)
                build = self.configured(**self.provisioned())
                for line in self.command_lines(build):
                    self.assertEqual(line[0], str(self.toolchain / "bin/clang++"))
                    self.assertEqual([t for t in line if is_sysroot_option(t)],
                                     ["--sysroot=" + str(self.sysroot)])
                    self.assertIn("-std=gnu++20", line)
                    self.assertIn("--target=wasm32-wasi", line)
                    self.assertNotIn(str(self.producer), " ".join(line))

    def test_engine_version_selects_the_runtime_semantics_version(self):
        # The C++ runtime is compiled for the bytecode semantics of the
        # table NIGHT_ENGINE_VERSION selects (runtime/NightEngineVersion.h
        # static_asserts it against the engine's own Opcodes.h).
        self.package_config()
        for version, semantics in (("ff147", 1), ("ff159", 2)):
            with self.subTest(version=version):
                build = self.configured(NIGHT_ENGINE_VERSION=version, **self.provisioned())
                compiles = json.loads((build / "compile_commands.json").read_text())
                runtime = [shlex.split(e["command"]) for e in compiles if "/runtime/" in e["file"]]
                self.assertTrue(runtime, "no runtime compile lines")
                for line in runtime:
                    self.assertIn(f"-DNIGHT_JSOP_SEMANTICS_VERSION={semantics}", line)

    def test_unknown_engine_version_fails_at_configure(self):
        self.package_config()
        _, result = self.configure(NIGHT_ENGINE_VERSION="ff999", **self.provisioned())
        self.assertNotEqual(result.returncode, 0, "configured for an engine without a table")
        self.assertIn("ff999", result.stderr)

    def test_package_config_without_toolchain_root_fails(self):
        for kind, write in (("package", self.package_config),
                            ("bare cc only", lambda: self.write_config([], bare_cc=True))):
            with self.subTest(config=kind):
                write()
                _, result = self.configure(NIGHT_SYSROOT=self.sysroot)
                self.assertNotEqual(result.returncode, 0, "configured with a clang from PATH")
                self.assertIn("NIGHT_TOOLCHAIN_ROOT", result.stderr)

    def test_both_compilers_resolve_under_the_root(self):
        for kind in ("legacy", "package"):
            with self.subTest(config=kind):
                self.legacy_config() if kind == "legacy" else self.package_config()
                build = self.configured(**self.provisioned())
                self.assertEqual(self.compiler(build, "C"), str(self.toolchain / "bin/clang"))
                self.assertEqual(self.compiler(build, "CXX"), str(self.toolchain / "bin/clang++"))

    def source_with_fixture_table(self):
        """A copy of the source tree whose ff147 table is the fixture's.

        gen_opcodes.py checks against the table next to itself, so it is
        copied (not linked) and generates the table there; everything else
        is linked to the real tree.
        """
        source = self.base / "source"
        (source / "scripts").mkdir(parents=True)
        for entry in SOURCE.iterdir():
            if entry.name not in ("scripts", "compiler", "target", "build"):
                (source / entry.name).symlink_to(entry)
        for entry in (SOURCE / "scripts").iterdir():
            if entry.name != "gen_opcodes.py":
                (source / "scripts" / entry.name).symlink_to(entry)
        shutil.copyfile(SOURCE / "scripts/gen_opcodes.py", source / "scripts/gen_opcodes.py")
        subprocess.run(["python3", str(source / "scripts/gen_opcodes.py"), "generate", "ff147",
                        str(self.opcodes_h)], check=True, capture_output=True)
        return source

    def build_opcodes_check(self, build):
        result = subprocess.run(
            ["cmake", "--build", str(build), "--target", "night_opcodes_check"],
            env=self.env, capture_output=True, text=True, check=False,
        )
        return result.returncode, result.stdout + result.stderr

    def test_opcode_check_refuses_a_different_opcodes_h(self):
        self.package_config()
        build = self.configured(self.source_with_fixture_table(), **self.provisioned())
        status, output = self.build_opcodes_check(build)
        self.assertEqual(status, 0, output)
        # The check is a build target, so the header is read at build time.
        # A comment changes only the digest, not the parsed table.
        self.opcodes_h.write_text(OPCODES_H + "// altered\n")
        status, output = self.build_opcodes_check(build)
        self.assertNotEqual(status, 0, output)
        self.assertIn(f"is not what {self.opcodes_h} generates", output)
        self.assertIn("OPCODES_H_SHA256", output)


if __name__ == "__main__":
    unittest.main()
