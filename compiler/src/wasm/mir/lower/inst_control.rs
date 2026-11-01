//! inst control: the lowering of one run of arms of `Lower::inst`, split out of `lower.rs` unchanged.

use super::*;

impl<'a> Lower<'a> {
    pub(super) fn inst_control(
        &mut self,
        inst: mir::Inst,
        d: &mir::func::InstData,
        a: &[Value],
    ) -> R<bool> {
        match d.op {
            // --- terminators ---
            Opcode::Jump => {
                let t = self.edge(inst, 0, &[])?;
                self.terminate(Terminator::Br { target: t });
            }
            Opcode::Br => {
                let (t, e) = (self.edge(inst, 0, &[])?, self.edge(inst, 1, &[])?);
                self.cond_br(a[0], t, e);
            }
            Opcode::Switch(n) => {
                let mut targets = vec![];
                for k in 0..n as usize {
                    targets.push(self.edge(inst, k, &[])?);
                }
                let default = self.edge(inst, n as usize, &[])?;
                self.terminate(Terminator::Select {
                    value: a[0],
                    targets,
                    default,
                });
            }
            Opcode::SwitchStr(n) => self.switch_str(inst, a, n)?,
            Opcode::StampFresh(w) => {
                let obj = self.un(Operator::I32WrapI64, a[0], Type::I32);
                let wv = self.i32c(w);
                self.store_i32(obj, OBJ_CLASS_IDX_OFFSET, wv);
            }
            Opcode::Restamp(i) => self.restamp(a[0], self.mm.restamps[i as usize]),
            Opcode::Return => {
                self.store_i64(self.retval_out, 0, a[0]);
                let z = self.i32c(0);
                self.ret(z);
            }
            Opcode::Unreachable => self.terminate(Terminator::Unreachable),
            Opcode::GenSuspend {
                index,
                nargs,
                nlocals,
                initial,
                ..
            } => {
                self.gen_suspend(inst, a, index, nargs, nlocals, initial)?;
            }
            #[cfg(feature = "ff147")]
            Opcode::IsGenClosing => {
                use crate::wasm::translate::MAGIC_GENERATOR_CLOSING;
                let m = self.i64c((TAG_MAGIC << 32) | MAGIC_GENERATOR_CLOSING);
                let r = self.bin(Operator::I64Eq, a[0], m, Type::I32);
                self.def(inst, r);
            }
            // 159 bytecode has no IsGenClosing (and no generator-closing
            // magic) to build it from.
            #[cfg(feature = "ff159")]
            Opcode::IsGenClosing => return Err("BUG: IsGenClosing in an ff159 body".into()),
            Opcode::Exit { pc, nargs, nlocals } | Opcode::ExitThrow { pc, nargs, nlocals } => {
                let mode = if matches!(d.op, Opcode::Exit { .. }) {
                    ResumeMode::Continue
                } else {
                    ResumeMode::Throw
                };
                let dead: Vec<bool> = d
                    .args
                    .iter()
                    .map(|&v| {
                        matches!(
                            self.f.values[v].def,
                            mir::func::ValueDef::Result(i, _)
                                if self.f.insts[i].op == Opcode::ConstVal(ConstVal::Dead)
                        )
                    })
                    .collect();
                let tys: Vec<MType> = d.args.iter().map(|&v| self.ty(v)).collect();
                self.exit(ResumeWord { pc, mode }, a, &tys, &dead, nargs, nlocals)?;
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
}
