"""Behavioral checks for the checked-in Rust opcode generator."""

import runpy
import subprocess
import tempfile
import unittest
from pathlib import Path


GENERATOR = runpy.run_path(str(Path(__file__).resolve().parents[1] / "gen_opcodes.py"))
OPCODES_DIR = Path(__file__).resolve().parents[2] / "compiler" / "src" / "opcodes"


def compile_and_run(test, source):
    """Compile `source` (a generated table plus a `main`) and run it."""
    with tempfile.TemporaryDirectory() as directory:
        generated = Path(directory) / "table.rs"
        executable = Path(directory) / "table"
        generated.write_text(source, encoding="utf-8")
        compiled = subprocess.run(
            ["rustc", "--edition", "2021", str(generated), "-o", str(executable)],
            capture_output=True,
            text=True,
            check=False,
        )
        test.assertEqual(compiled.returncode, 0, compiled.stdout + compiled.stderr)
        executed = subprocess.run([str(executable)], capture_output=True, text=True, check=False)
        test.assertEqual(executed.returncode, 0, executed.stdout + executed.stderr)


# The entries Firefox 159 (JSOP_SEMANTICS_VERSION 2) changed, as its
# Opcodes.h spells them, in table order with an unchanged neighbour.
FF159_HEADER = """#define JSOP_SEMANTICS_VERSION 2
#define FOR_EACH_OPCODE(MACRO) \\
    MACRO(NewTarget, new_target, NULL, 1, 0, 1, JOF_BYTE) \\
    MACRO(DynamicImport, dynamic_import, NULL, 2, 2, 1, JOF_UINT8) \\
    MACRO(InitialYield, initial_yield, NULL, 4, 1, 2, JOF_RESUMEINDEX) \\
    MACRO(Yield, yield, NULL, 4, 2, 2, JOF_RESUMEINDEX) \\
    MACRO(Await, await, NULL, 4, 2, 2, JOF_RESUMEINDEX) \\
    MACRO(CreateSuppressedError, create_suppressed_error, NULL, 1, 2, 1, JOF_BYTE) \\
    MACRO(AddDisposable, add_disposable, NULL, 2, 3, 0, JOF_UINT8|JOF_USES_ENV) \\
    MACRO(TakeDisposeCapability, take_dispose_capability, NULL, 1, 0, 1, JOF_BYTE|JOF_USES_ENV)
#define FOR_EACH_TRAILING_UNUSED_OPCODE(MACRO)
"""


class OpcodeGeneratorTests(unittest.TestCase):
    def test_semantics_version_is_recorded(self):
        header = FF159_HEADER
        source = GENERATOR["render"]("ff159", header, GENERATOR["parse_opcodes"](header))
        source += """
fn main() {
    assert_eq!(JSOP_SEMANTICS_VERSION, 2);
}
"""
        compile_and_run(self, source)

    def test_header_without_semantics_version_is_refused(self):
        header = FF159_HEADER.replace("#define JSOP_SEMANTICS_VERSION 2\n", "")
        with self.assertRaises(SystemExit) as raised:
            GENERATOR["render"]("ff159", header, GENERATOR["parse_opcodes"](header))
        self.assertIn("JSOP_SEMANTICS_VERSION", str(raised.exception))

    def test_ff159_entries(self):
        # Numbering, lengths, stack effects and operand formats of the
        # entries 159 changed, and the explicit-resource-management ops now
        # that no build flag gates them.
        header = FF159_HEADER
        source = GENERATOR["render"]("ff159", header, GENERATOR["parse_opcodes"](header))
        source += """
fn main() {
    assert_eq!(JSOp::from_byte(1), Some(JSOp::DynamicImport));
    assert_eq!(JSOp::DynamicImport.len(), 2);
    assert_eq!(JSOp::DynamicImport.format(), "UINT8");
    for op in [JSOp::InitialYield, JSOp::Yield, JSOp::Await] {
        assert_eq!(op.ndefs(), 2, "{op:?}");
    }
    assert_eq!(JSOp::from_byte(5), Some(JSOp::CreateSuppressedError));
    assert_eq!(JSOp::from_byte(6), Some(JSOp::AddDisposable));
    assert_eq!(JSOp::AddDisposable.format(), "UINT8");
    assert_eq!(JSOp::from_byte(7), Some(JSOp::TakeDisposeCapability));
    assert_eq!(JSOp::from_byte(8), None);
}
"""
        compile_and_run(self, source)

    def test_checked_in_tables_record_their_engine(self):
        # Each checked-in table names its version, the header digest it was
        # generated from and that header's semantics version.
        expected = {
            "ff147": ("6d263a46984a467aa1eb3a31b0f27edd25caccdfd853e4479788e914f4922499", 1),
            "ff159": ("ace0a53d231375492c36142be7c64043de38a17b1752ad2dc32644715ea84c0b", 2),
        }
        for version, (digest, semantics) in expected.items():
            with self.subTest(version=version):
                table = (OPCODES_DIR / f"{version}.rs").read_text(encoding="utf-8")
                for line in (
                    f'pub const ENGINE_VERSION: &str = "{version}";',
                    f'    "{digest}";',
                    f"pub const JSOP_SEMANTICS_VERSION: u32 = {semantics};",
                ):
                    self.assertTrue(line in table.splitlines(), f"{version}.rs lacks {line!r}")

    def test_operand_format_reports_type_without_flags(self):
        header = """#define JSOP_SEMANTICS_VERSION 1
#define FOR_EACH_OPCODE(MACRO) \\
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
        header = """#define JSOP_SEMANTICS_VERSION 1
#define FOR_EACH_OPCODE(MACRO) \\
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
