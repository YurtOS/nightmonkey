//! op consts: the lowering of one run of arms of `Gen::op`, split out of `codegen.rs` unchanged.

use super::*;

impl<'a> Gen<'a> {
    pub(super) fn op_consts(&mut self, p: &mut BytecodeParser, op: JSOp) -> R<bool> {
        use JSOp::*;
        let d = self.d;
        let h = self.h;
        match op {
            Nop | Lineno | JumpTarget | LoopHead | Debugger | NopDestructuring | NopIsAssignOp
            | TryDestructuring | DebugLeaveLexicalEnv | Finally | Try | DebugCheckSelfHosted => {
                skip(p, op);
            }

            // --- constants ---
            Undefined => self.push_const(d, UNDEF),
            Null => self.push_const(d, TAG_NULL << 32),
            False => self.push_const(d, TAG_BOOLEAN << 32),
            True => self.push_const(d, (TAG_BOOLEAN << 32) | 1),
            Zero => self.push_const(d, TAG_INT32 << 32),
            One => self.push_const(d, (TAG_INT32 << 32) | 1),
            Int8 => {
                let i = p.next_int8().unwrap();
                self.push_const(d, (TAG_INT32 << 32) | u64::from(i32::from(i) as u32));
            }
            Uint16 => {
                let i = p.next_uint16().unwrap();
                self.push_const(d, (TAG_INT32 << 32) | u64::from(i));
            }
            Uint24 => {
                let i = p.next_uint24().unwrap();
                self.push_const(d, (TAG_INT32 << 32) | u64::from(i));
            }
            Int32 => {
                let i = p.next_int32().unwrap();
                self.push_const(d, (TAG_INT32 << 32) | u64::from(i as u32));
            }
            Double => {
                let bits = p.next_uint64().unwrap();
                self.push_const(d, bits);
            }
            Hole => self.push_const(d, (TAG_MAGIC << 32) | MAGIC_ELEMENTS_HOLE),
            Uninitialized => self.push_const(d, (TAG_MAGIC << 32) | MAGIC_UNINITIALIZED_LEXICAL),
            IsConstructing => self.push_const(d, (TAG_MAGIC << 32) | MAGIC_IS_CONSTRUCTING),
            Void => self.push_const(d - 1, UNDEF),
            String => {
                let a = self.atom(p.next_uint32().unwrap())?;
                let av = self.i32c(a);
                let r = self.rt(h.string, &[av]);
                self.set_slot(d, r);
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
}
