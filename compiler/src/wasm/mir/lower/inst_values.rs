//! inst values: the lowering of one run of arms of `Lower::inst`, split out of `lower.rs` unchanged.

use super::*;

impl<'a> Lower<'a> {
    pub(super) fn inst_values(
        &mut self,
        inst: mir::Inst,
        d: &mir::func::InstData,
        a: &[Value],
    ) -> R<bool> {
        let at = |i: usize| self.f.values[d.args[i]].ty;
        match d.op {
            Opcode::ConstVal(c) => {
                let bits = match c {
                    ConstVal::Undefined => UNDEF,
                    ConstVal::Null => TAG_NULL << 32,
                    ConstVal::Bool(b) => (TAG_BOOLEAN << 32) | u64::from(b),
                    ConstVal::Int32(n) => (TAG_INT32 << 32) | u64::from(n as u32),
                    ConstVal::Double(bits) => bits,
                    ConstVal::Uninitialized => (TAG_MAGIC << 32) | MAGIC_UNINITIALIZED_LEXICAL,
                    ConstVal::IsConstructing => (TAG_MAGIC << 32) | MAGIC_IS_CONSTRUCTING,
                    ConstVal::Dead => UNDEF,
                    ConstVal::Hole => {
                        (TAG_MAGIC << 32) | crate::wasm::translate::MAGIC_ELEMENTS_HOLE
                    }
                };
                let v = self.i64c(bits);
                self.def(inst, v);
            }
            Opcode::ConstI32(n) => {
                let v = self.i32c(n as u32);
                self.def(inst, v);
            }
            Opcode::ConstF64(bits) => {
                let v = self.f64c(bits);
                self.def(inst, v);
            }
            Opcode::ConstBool(b) => {
                let v = self.i32c(u32::from(b));
                self.def(inst, v);
            }
            Opcode::Box => {
                let v = self.boxed(&at(0), a[0])?;
                self.def(inst, v);
            }
            Opcode::BoxDouble => {
                let bits = self.un(Operator::I64ReinterpretF64, a[0], Type::I64);
                let nan = self.bin(Operator::F64Ne, a[0], a[0], Type::I32);
                let canon = self.i64c(CANONICAL_NAN_BITS);
                let v = self.select(Type::I64, canon, bits, nan);
                self.def(inst, v);
            }
            Opcode::Unbox(k) => {
                let v = match k {
                    UnboxKind::F64Num => self.to_f64(a[0]),
                    _ => self.un(Operator::I32WrapI64, a[0], Type::I32),
                };
                self.def(inst, v);
            }
            Opcode::Weaken => self.def(inst, a[0]),
            Opcode::I32ToInt => {
                let v = self.un(Operator::I64ExtendI32S, a[0], Type::I64);
                self.def(inst, v);
            }
            Opcode::I32ToF64 => {
                let v = self.un(Operator::F64ConvertI32S, a[0], Type::F64);
                self.def(inst, v);
            }
            Opcode::IntToF64 => {
                let v = self.un(Operator::F64ConvertI64S, a[0], Type::F64);
                self.def(inst, v);
            }
            Opcode::I32Wrap(op) => {
                let o = match op {
                    ArithOp::Add => Operator::I32Add,
                    ArithOp::Sub => Operator::I32Sub,
                    ArithOp::Mul => Operator::I32Mul,
                    // Its type rules out a zero divisor.
                    ArithOp::Rem => Operator::I32RemS,
                };
                let v = self.bin(o, a[0], a[1], Type::I32);
                self.def(inst, v);
            }
            Opcode::IntArith(op) => {
                let o = match op {
                    ArithOp::Add => Operator::I64Add,
                    ArithOp::Sub => Operator::I64Sub,
                    ArithOp::Mul => Operator::I64Mul,
                    ArithOp::Rem => Operator::I64RemS,
                };
                let v = self.bin(o, a[0], a[1], Type::I64);
                self.def(inst, v);
            }
            Opcode::F64Arith(op) => {
                let o = match op {
                    F64Op::Add => Operator::F64Add,
                    F64Op::Sub => Operator::F64Sub,
                    F64Op::Mul => Operator::F64Mul,
                    F64Op::Div => Operator::F64Div,
                    // Wasm has no fmod: the leaf helper (`js::NumberMod`).
                    F64Op::Mod => {
                        let v = self.call1(self.h.fmod, &[a[0], a[1]], Type::F64);
                        self.def(inst, v);
                        return Ok(true);
                    }
                };
                let v = self.bin(o, a[0], a[1], Type::F64);
                self.def(inst, v);
            }
            Opcode::F64Neg => {
                let v = self.un(Operator::F64Neg, a[0], Type::F64);
                self.def(inst, v);
            }
            Opcode::I32Bit(op) => {
                let o = match op {
                    BitOp::And => Operator::I32And,
                    BitOp::Or => Operator::I32Or,
                    BitOp::Xor => Operator::I32Xor,
                    // Wasm masks shift counts to 5 bits, as JS does.
                    BitOp::Shl => Operator::I32Shl,
                    BitOp::Shr => Operator::I32ShrS,
                };
                let v = self.bin(o, a[0], a[1], Type::I32);
                self.def(inst, v);
            }
            Opcode::I32Ushr => {
                let r = self.bin(Operator::I32ShrU, a[0], a[1], Type::I32);
                let v = self.un(Operator::I64ExtendI32U, r, Type::I64);
                self.def(inst, v);
            }
            Opcode::Cmp(repr, cc) => {
                use Operator::*;
                let o = match (repr, cc) {
                    (NumRepr::I32, Cc::Eq) => I32Eq,
                    (NumRepr::I32, Cc::Ne) => I32Ne,
                    (NumRepr::I32, Cc::Lt) => I32LtS,
                    (NumRepr::I32, Cc::Le) => I32LeS,
                    (NumRepr::I32, Cc::Gt) => I32GtS,
                    (NumRepr::I32, Cc::Ge) => I32GeS,
                    (NumRepr::Int, Cc::Eq) => I64Eq,
                    (NumRepr::Int, Cc::Ne) => I64Ne,
                    (NumRepr::Int, Cc::Lt) => I64LtS,
                    (NumRepr::Int, Cc::Le) => I64LeS,
                    (NumRepr::Int, Cc::Gt) => I64GtS,
                    (NumRepr::Int, Cc::Ge) => I64GeS,
                    (NumRepr::F64, Cc::Eq) => F64Eq,
                    (NumRepr::F64, Cc::Ne) => F64Ne,
                    (NumRepr::F64, Cc::Lt) => F64Lt,
                    (NumRepr::F64, Cc::Le) => F64Le,
                    (NumRepr::F64, Cc::Gt) => F64Gt,
                    (NumRepr::F64, Cc::Ge) => F64Ge,
                };
                let v = self.bin(o, a[0], a[1], Type::I32);
                self.def(inst, v);
            }
            Opcode::Math(m) => {
                let v = match m {
                    MathFn::Abs => self.un(Operator::F64Abs, a[0], Type::F64),
                    MathFn::Floor => self.un(Operator::F64Floor, a[0], Type::F64),
                    MathFn::Ceil => self.un(Operator::F64Ceil, a[0], Type::F64),
                    MathFn::Trunc => self.un(Operator::F64Trunc, a[0], Type::F64),
                    MathFn::Sqrt => self.un(Operator::F64Sqrt, a[0], Type::F64),
                    MathFn::Fround => {
                        let f = self.un(Operator::F32DemoteF64, a[0], Type::F32);
                        self.un(Operator::F64PromoteF32, f, Type::F64)
                    }
                    // wasm's min/max are JS's: NaN wins, -0 < +0.
                    MathFn::Min => self.bin(Operator::F64Min, a[0], a[1], Type::F64),
                    MathFn::Max => self.bin(Operator::F64Max, a[0], a[1], Type::F64),
                    MathFn::Pow => self.call1(self.h.math_pow, &[a[0], a[1]], Type::F64),
                    MathFn::Sin | MathFn::Cos => {
                        let k = self.i32c(u32::from(m == MathFn::Cos));
                        self.call1(self.h.math_unary, &[k, a[0]], Type::F64)
                    }
                    _ => return Err(format!("lowering: math {m:?}")),
                };
                self.def(inst, v);
            }
            Opcode::CheckNative(n) => {
                // The callee's JSNative is the pristine one (a function
                // object's native sits in its env slot's place, as
                // `math_arms` reads it).
                let idx = native_math_index(&self.mm.natives[n].name)
                    .ok_or_else(|| format!("lowering: native {}", self.mm.natives[n].name))?;
                let (_, _, native) = self.classify_native(a[0]);
                let fail = self.body.add_block();
                self.check(native, fail);
                let cp = self.un(Operator::I32WrapI64, a[0], Type::I32);
                let nf = self.load_i32(cp, FUNC_ENV_SLOT_OFFSET);
                let c = self.i32c(self.h.math_natives_base + 4 * idx);
                let slot = self.load_i32(c, 0);
                let same = self.bin(Operator::I32Eq, nf, slot, Type::I32);
                let (ok_b, cont) = (self.body.add_block(), self.body.add_block());
                self.cond_br(same, Self::to(ok_b), Self::to(fail));
                self.cur = fail;
                self.terminate(Terminator::Br {
                    target: Self::to(cont),
                });
                self.cur = ok_b;
                let t = self.edge(inst, 0, &[])?;
                self.terminate(Terminator::Br { target: t });
                self.cur = cont;
                let e = self.edge(inst, 1, &[])?;
                self.terminate(Terminator::Br { target: e });
            }
            Opcode::ToInt32 => {
                let x = match at(0) {
                    MType::Int(_) => self.un(Operator::F64ConvertI64S, a[0], Type::F64),
                    _ => a[0],
                };
                let v = self.to_int32(x);
                self.def(inst, v);
            }
            Opcode::JsToBool => {
                let v = self.to_bool(a[0]);
                self.def(inst, v);
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
}
