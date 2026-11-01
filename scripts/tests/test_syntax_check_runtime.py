"""The runtime syntax check's command rewriting."""

import os
import runpy
import tempfile
import unittest
from pathlib import Path

CHECK = runpy.run_path(str(Path(__file__).resolve().parents[1] / "syntax-check-runtime.py"))


class CommandTests(unittest.TestCase):
    def test_command_string_and_joined_output(self):
        entry = {"directory": "/b", "file": "/s/a.cpp",
                 "command": "/cc/clang++ -DX -oout.o -c /s/a.cpp"}
        self.assertEqual(CHECK["command_for"](entry, []),
                         ["/cc/clang++", "-DX", "/s/a.cpp", "-fsyntax-only", "-ferror-limit=50"])

    def test_arguments_entries(self):
        entry = {"directory": "/b", "file": "/s/a.cpp",
                 "arguments": ["/cc/clang++", "-DX", "-o", "a.o", "-c", "/s/a.cpp"]}
        self.assertEqual(CHECK["command_for"](entry, [])[:3], ["/cc/clang++", "-DX", "/s/a.cpp"])

    def test_overlay_goes_after_a_launcher(self):
        entry = {"directory": "/b", "file": "/s/a.cpp",
                 "arguments": ["ccache", "/cc/clang++", "-DX", "-c", "/s/a.cpp"]}
        self.assertEqual(CHECK["command_for"](entry, ["/inc"])[:4],
                         ["ccache", "/cc/clang++", "-I/inc", "-DX"])

    def test_include_dirs_resolve_and_must_exist(self):
        with tempfile.TemporaryDirectory() as d:
            old = os.getcwd()
            os.chdir(d)
            try:
                Path("inc").mkdir()
                self.assertEqual(CHECK["include_dirs"](["inc"]), [str(Path(d, "inc").resolve())])
                with self.assertRaises(SystemExit):
                    CHECK["include_dirs"](["missing"])
            finally:
                os.chdir(old)


if __name__ == "__main__":
    unittest.main()
