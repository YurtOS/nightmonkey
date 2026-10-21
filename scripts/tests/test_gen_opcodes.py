"""Behavioral checks for the checked-in Rust opcode generator."""

import runpy
import subprocess
import tempfile
import unittest
from pathlib import Path


GENERATOR = runpy.run_path(str(Path(__file__).resolve().parents[1] / "gen_opcodes.py"))


class OpcodeGeneratorTests(unittest.TestCase):
    def test_operand_format_reports_type_without_flags(self):
        header = """#define FOR_EACH_OPCODE(MACRO) \\
  MACRO(GetArg, getarg, NULL, 3, 0, 1, JOF_QARG | JOF_NAME) \\
  MACRO(Goto, goto_, NULL, 5, 0, 0, JOF_JUMP | JOF_IC)
#define FOR_EACH_TRAILING_UNUSED_OPCODE(MACRO)
"""
        source = GENERATOR["render"]("ff147", header, GENERATOR["parse_opcodes"](header))
        source += """
fn main() {
    assert_eq!(JSOp::GetArg.format(), "QARG");
    assert_eq!(JSOp::Goto.format(), "JUMP");
}
"""
        with tempfile.TemporaryDirectory() as directory:
            generated = Path(directory) / "operand_formats.rs"
            executable = Path(directory) / "operand_formats"
            generated.write_text(source, encoding="utf-8")
            compiled = subprocess.run(
                ["rustc", "--edition", "2021", str(generated), "-o", str(executable)],
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertEqual(compiled.returncode, 0, compiled.stdout + compiled.stderr)
            executed = subprocess.run(
                [str(executable)], capture_output=True, text=True, check=False
            )
            self.assertEqual(executed.returncode, 0, executed.stdout + executed.stderr)

    def test_rendered_opcode_table_passes_rustfmt(self):
        # The digest declaration must be formatted by the generator itself:
        # reformatting only the checked-in output breaks engine-table checks.
        header = """#define FOR_EACH_OPCODE(MACRO) \\
  MACRO(Undefined, undefined, NULL, 1, 0, 1, JOF_BYTE) \\
  MACRO(Pop, pop, NULL, 1, 1, 0, JOF_BYTE)
#define FOR_EACH_TRAILING_UNUSED_OPCODE(MACRO)
"""
        source = GENERATOR["render"]("ff147", header, GENERATOR["parse_opcodes"](header))
        with tempfile.TemporaryDirectory() as directory:
            generated = Path(directory) / "opcodes.rs"
            generated.write_text(source, encoding="utf-8")
            result = subprocess.run(
                ["rustfmt", "--check", "--edition", "2021", str(generated)],
                capture_output=True,
                text=True,
                check=False,
            )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)


if __name__ == "__main__":
    unittest.main()
