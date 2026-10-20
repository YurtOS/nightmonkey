//! inst guards: the lowering of one run of arms of `Lower::inst`, split out of `lower.rs` unchanged.

use super::*;

impl<'a> Lower<'a> {
    pub(super) fn inst_guards(
        &mut self,
        inst: mir::Inst,
        d: &mir::func::InstData,
        a: &[Value],
    ) -> R<bool> {
        match d.op {
            Opcode::GuardUnbox(UnboxKind::F64Num) if self.plain_guard(inst) => {
                let tag = self.tag_of(a[0]);
                let have = value_tags(&self.ty(d.args[0]));
                self.unbox_num_edges(inst, a[0], tag, have)?;
            }
            Opcode::GuardUnbox(k) => {
                let v = a[0];
                let tag = self.tag_of(v);
                let (cond, out) = match k {
                    UnboxKind::I32 => {
                        let c = self.tag_is(tag, TAG_INT32 as u32);
                        (c, self.un(Operator::I32WrapI64, v, Type::I32))
                    }
                    UnboxKind::F64Num => {
                        let k = self.i32c(TAG_INT32 as u32);
                        let c = self.bin(Operator::I32LeU, tag, k, Type::I32);
                        (c, self.to_f64(v))
                    }
                    UnboxKind::Bool => {
                        let c = self.tag_is(tag, TAG_BOOLEAN as u32);
                        (c, self.un(Operator::I32WrapI64, v, Type::I32))
                    }
                    UnboxKind::Obj => {
                        let c = self.tag_is(tag, TAG_OBJECT as u32);
                        (c, self.un(Operator::I32WrapI64, v, Type::I32))
                    }
                    UnboxKind::Str => {
                        let c = self.tag_is(tag, TAG_STRING as u32);
                        (c, self.un(Operator::I32WrapI64, v, Type::I32))
                    }
                };
                self.guard(inst, cond, &[out])?;
            }
            Opcode::GuardTags(t) => {
                // Of the tags the input may have, test the fewer: those
                // in `t`, or those outside it.
                let have = match self.ty(d.args[0]) {
                    MType::Val(s) => s.tags,
                    _ => TagSet::ALL,
                };
                let (inside, outside) = (have.intersect(t), have.minus(t));
                let c = if outside.count() < inside.count() {
                    let o = self.has_tags(a[0], outside);
                    self.un(Operator::I32Eqz, o, Type::I32)
                } else {
                    self.has_tags(a[0], inside)
                };
                self.guard(inst, c, &[a[0]])?;
            }
            Opcode::F64ToIntExact => {
                let x = a[0];
                let i = self.un(Operator::I32TruncSatF64S, x, Type::I32);
                let back = self.un(Operator::F64ConvertI32S, i, Type::F64);
                let exact = self.bin(Operator::F64Eq, back, x, Type::I32);
                let bits = self.un(Operator::I64ReinterpretF64, x, Type::I64);
                let negz = self.i64c(1 << 63);
                let not_negz = self.bin(Operator::I64Ne, bits, negz, Type::I32);
                let ok = self.bin(Operator::I32And, exact, not_negz, Type::I32);
                self.guard(inst, ok, &[i])?;
            }
            Opcode::I32Ovf(op @ (ArithOp::Add | ArithOp::Sub))
                if self
                    .const_i32(d.args[1])
                    .or(if op == ArithOp::Add {
                        self.const_i32(d.args[0])
                    } else {
                        None
                    })
                    .is_some() =>
            {
                // `x + c` (either side) or `x - c`: overflow is `x` past
                // one bound, one compare rather than a widened sum.
                let (x, c) = match self.const_i32(d.args[1]) {
                    Some(c) => (a[0], c),
                    None => (a[1], self.const_i32(d.args[0]).unwrap()),
                };
                let c = if op == ArithOp::Sub { -c } else { c };
                let o = if op == ArithOp::Add {
                    Operator::I32Add
                } else {
                    Operator::I32Sub
                };
                let r = self.bin(o, a[0], a[1], Type::I32);
                let ok = if c >= 0 {
                    let bound = self.i32c((i64::from(i32::MAX) - c) as i32 as u32);
                    self.bin(Operator::I32LeS, x, bound, Type::I32)
                } else {
                    let bound = self.i32c((i64::from(i32::MIN) - c) as i32 as u32);
                    self.bin(Operator::I32GeS, x, bound, Type::I32)
                };
                self.guard(inst, ok, &[r])?;
            }
            Opcode::I32Ovf(op) => {
                let (x, y) = (a[0], a[1]);
                let (r, ok) = match op {
                    ArithOp::Add | ArithOp::Sub => {
                        let (o, wide) = if op == ArithOp::Add {
                            (Operator::I32Add, Operator::I64Add)
                        } else {
                            (Operator::I32Sub, Operator::I64Sub)
                        };
                        let r = self.bin(o, x, y, Type::I32);
                        let x64 = self.un(Operator::I64ExtendI32S, x, Type::I64);
                        let y64 = self.un(Operator::I64ExtendI32S, y, Type::I64);
                        let w = self.bin(wide, x64, y64, Type::I64);
                        let r64 = self.un(Operator::I64ExtendI32S, r, Type::I64);
                        (r, self.bin(Operator::I64Eq, w, r64, Type::I32))
                    }
                    ArithOp::Mul => {
                        // Branches, not one flag: the product fits, and
                        // only a zero one (rare) tests for -0 (a negative
                        // operand).
                        let x64 = self.un(Operator::I64ExtendI32S, x, Type::I64);
                        let y64 = self.un(Operator::I64ExtendI32S, y, Type::I64);
                        let w = self.bin(Operator::I64Mul, x64, y64, Type::I64);
                        let r = self.un(Operator::I32WrapI64, w, Type::I32);
                        let r64 = self.un(Operator::I64ExtendI32S, r, Type::I64);
                        let fits = self.bin(Operator::I64Eq, w, r64, Type::I32);
                        let (nz, zero, ok_b, fail_b) = (
                            self.body.add_block(),
                            self.body.add_block(),
                            self.body.add_block(),
                            self.body.add_block(),
                        );
                        self.cond_br(fits, Self::to(nz), Self::to(fail_b));
                        self.cur = nz;
                        self.cond_br(r, Self::to(ok_b), Self::to(zero));
                        self.cur = zero;
                        let xy = self.bin(Operator::I32Or, x, y, Type::I32);
                        let z = self.i32c(0);
                        let neg = self.bin(Operator::I32LtS, xy, z, Type::I32);
                        self.cond_br(neg, Self::to(fail_b), Self::to(ok_b));
                        self.cur = fail_b;
                        let f = self.edge(inst, 1, &[])?;
                        self.terminate(Terminator::Br { target: f });
                        self.cur = ok_b;
                        (r, self.i32c(1))
                    }
                    ArithOp::Rem => {
                        // A zero divisor fails (NaN), and so does a zero
                        // remainder of a negative dividend (-0). `rem_s`
                        // traps on 0, so it divides by 1 there instead;
                        // INT_MIN % -1 is 0 in Wasm, and fails as -0.
                        let z = self.i32c(0);
                        let nz = self.bin(Operator::I32Ne, y, z, Type::I32);
                        let one = self.i32c(1);
                        let d = self.select(Type::I32, y, one, nz);
                        let r = self.bin(Operator::I32RemS, x, d, Type::I32);
                        let neg = self.bin(Operator::I32LtS, x, z, Type::I32);
                        let rz = self.un(Operator::I32Eqz, r, Type::I32);
                        let negz = self.bin(Operator::I32And, neg, rz, Type::I32);
                        let pos = self.un(Operator::I32Eqz, negz, Type::I32);
                        (r, self.bin(Operator::I32And, nz, pos, Type::I32))
                    }
                };
                self.guard(inst, ok, &[r])?;
            }
            Opcode::Prim(p) => self.prim_op(inst, p, a)?,
            Opcode::JsAdd
            | Opcode::JsBinop(_)
            | Opcode::JsUnop(_)
            | Opcode::JsCompare(_)
            | Opcode::JsToNumeric => {
                self.numeric_fast_arms(inst, &d.op, a)?;
                if d.op == Opcode::JsAdd {
                    self.concat_arm(inst, d, a)?;
                }
                self.js_op(inst, &d.op, a)?
            }
            Opcode::ArgsMapped(n) | Opcode::ArgsMappedSet(n) => {
                // The entry made the object (mapped scripts are not
                // inlined: the frame is the function's own).
                let obj = self.load_i64(self.vp, self.layout.args_obj());
                let i = self.i32c(n);
                if let Opcode::ArgsMapped(_) = d.op {
                    let v = self.call1(self.h.get_mapped_arg, &[obj, i], Type::I64);
                    self.def(inst, v);
                } else {
                    self.call(self.h.set_mapped_arg, &[obj, i, a[0]], &[]);
                }
            }
            Opcode::JsTypeofEq(k) => {
                let v = self.typeof_eq(a[0], k);
                self.def(inst, v);
            }
            Opcode::JsConstantStrictEq(k) => {
                // `ConstantStrictEqual` inline: the operand's type
                // byte, then its payload.
                let v = self.constant_strict_eq(a[0], k);
                self.def(inst, v);
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
}
