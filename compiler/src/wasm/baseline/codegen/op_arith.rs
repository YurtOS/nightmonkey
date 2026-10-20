//! op arith: the lowering of one run of arms of `Gen::op`, split out of `codegen.rs` unchanged.

use super::*;

impl<'a> Gen<'a> {
    pub(super) fn op_arith(&mut self, p: &mut BytecodeParser, op: JSOp) -> R<bool> {
        use JSOp::*;
        let d = self.d;
        let h = self.h;
        match op {
            // --- arithmetic and comparison ---
            // Inline arms as in the portable baseline interpreter: int32
            // operands, then any numbers (as doubles), then the helper
            // (`number_arms`).
            Add | Sub | Mul | Div | Mod | BitAnd | BitOr | BitXor | Lsh | Rsh | Ursh => {
                let a = self.slot(d - 2);
                let b = self.slot(d - 1);
                let int: Option<Int32Op<'a>> = match op {
                    Add => Some(Self::add_i32),
                    Sub => Some(Self::sub_i32),
                    Mul => Some(Self::mul_i32),
                    Mod => Some(Self::mod_i32),
                    BitAnd => Some(|g, x, y, _| g.binop(Operator::I32And, x, y, Type::I32)),
                    BitOr => Some(|g, x, y, _| g.binop(Operator::I32Or, x, y, Type::I32)),
                    BitXor => Some(|g, x, y, _| g.binop(Operator::I32Xor, x, y, Type::I32)),
                    // Wasm masks shift counts to 5 bits, as JS does.
                    Lsh => Some(|g, x, y, _| g.binop(Operator::I32Shl, x, y, Type::I32)),
                    Rsh => Some(|g, x, y, _| g.binop(Operator::I32ShrS, x, y, Type::I32)),
                    Ursh => Some(Self::ursh_i32),
                    _ => None,
                };
                // Double `%` is fmod, which Wasm lacks: the helper does it.
                let num = match op {
                    Add => Some(Operator::F64Add),
                    Sub => Some(Operator::F64Sub),
                    Mul => Some(Operator::F64Mul),
                    Div => Some(Operator::F64Div),
                    _ => None,
                };
                let int_arm = int.map(|f| {
                    move |g: &mut Self, v: &[Value], bail| {
                        let r = f(g, v[0], v[1], bail);
                        g.boxed_int32(r)
                    }
                });
                let num_arm = num.map(|o| {
                    move |g: &mut Self, v: &[Value]| {
                        let r = g.binop(o, v[0], v[1], Type::F64);
                        g.box_number(r)
                    }
                });
                self.number_arms(
                    &[a, b],
                    int_arm.as_ref().map(|f| f as &IntArm<'_, 'a>),
                    num_arm.as_ref().map(|f| f as &NumArm<'_, 'a>),
                    |g| {
                        let r = if op == Add {
                            g.rt(h.add, &[a, b])
                        } else {
                            let k = g.i32c(binop_kind(op));
                            g.rt(h.binop, &[k, a, b])
                        };
                        g.set_slot(d - 2, r);
                    },
                );
            }
            Inc | Dec | Neg | BitNot => {
                let a = self.slot(d - 1);
                let int_arm = |g: &mut Self, v: &[Value], bail| {
                    let x = v[0];
                    let r = if op == BitNot {
                        let m1 = g.i32c(u32::MAX);
                        g.binop(Operator::I32Xor, x, m1, Type::I32)
                    } else {
                        // Inc and Dec overflow only from the extreme value;
                        // Neg of 0 is -0 and of INT32_MIN overflows.
                        let (limit, bin) = match op {
                            Inc => (i32::MAX as u32, Operator::I32Add),
                            Dec => (i32::MIN as u32, Operator::I32Sub),
                            _ => (i32::MIN as u32, Operator::I32Sub),
                        };
                        let l = g.i32c(limit);
                        let ovf = g.binop(Operator::I32Eq, x, l, Type::I32);
                        g.bail_if(ovf, bail);
                        if op == Neg {
                            let zero = g.unop(Operator::I32Eqz, x, Type::I32);
                            g.bail_if(zero, bail);
                            let z = g.i32c(0);
                            g.binop(Operator::I32Sub, z, x, Type::I32)
                        } else {
                            let one = g.i32c(1);
                            g.binop(bin, x, one, Type::I32)
                        }
                    };
                    g.boxed_int32(r)
                };
                let num_arm = |g: &mut Self, v: &[Value]| {
                    let r = match op {
                        Neg => g.unop(Operator::F64Neg, v[0], Type::F64),
                        _ => {
                            let one = g.f64c(1.0);
                            let bin = if op == Inc {
                                Operator::F64Add
                            } else {
                                Operator::F64Sub
                            };
                            g.binop(bin, v[0], one, Type::F64)
                        }
                    };
                    g.box_number(r)
                };
                self.number_arms(
                    &[a],
                    Some(&int_arm),
                    (op != BitNot).then_some(&num_arm as &NumArm<'_, 'a>),
                    |g| {
                        let r = match op {
                            Neg => g.rt(h.neg, &[a]),
                            _ => {
                                let k = g.i32c(binop_kind(op));
                                g.rt(h.binop, &[k, a, a])
                            }
                        };
                        g.set_slot(d - 1, r);
                    },
                );
            }
            Pow => self.binary(h.pow, &[], d),
            Lt | Gt | Le | Ge => {
                let (kind, icmp, fcmp) = match op {
                    Lt => (CMP_LT, Operator::I32LtS, Operator::F64Lt),
                    Gt => (CMP_GT, Operator::I32GtS, Operator::F64Gt),
                    Le => (CMP_LE, Operator::I32LeS, Operator::F64Le),
                    _ => (CMP_GE, Operator::I32GeS, Operator::F64Ge),
                };
                let a = self.slot(d - 2);
                let b = self.slot(d - 1);
                self.number_arms(
                    &[a, b],
                    Some(&|g: &mut Self, v: &[Value], _| {
                        let r = g.binop(icmp, v[0], v[1], Type::I32);
                        g.boxed_bool(r)
                    }),
                    Some(&|g: &mut Self, v: &[Value]| {
                        // False on NaN, as JS wants.
                        let r = g.binop(fcmp, v[0], v[1], Type::I32);
                        g.boxed_bool(r)
                    }),
                    |g| g.compare(kind, a, b),
                );
            }
            Eq | Ne | StrictEq | StrictNe => {
                let (kind, negate) = match op {
                    Eq => (CMP_EQ, false),
                    Ne => (CMP_NE, true),
                    StrictEq => (CMP_STRICTEQ, false),
                    _ => (CMP_STRICTNE, true),
                };
                let a = self.slot(d - 2);
                let b = self.slot(d - 1);
                let (inline, eq) = self.equality_inline(a, b);
                self.fast_path(
                    inline,
                    |g, _| {
                        let r = if negate {
                            g.unop(Operator::I32Eqz, eq, Type::I32)
                        } else {
                            eq
                        };
                        let r = g.boxed_bool(r);
                        g.set_slot(d - 2, r);
                    },
                    // Two numbers (at least one a double): loose and strict
                    // equality are both IEEE equality (NaN unequal, -0 == 0).
                    |g| {
                        g.number_arms(
                            &[a, b],
                            None,
                            Some(&|g: &mut Self, v: &[Value]| {
                                let o = if negate {
                                    Operator::F64Ne
                                } else {
                                    Operator::F64Eq
                                };
                                let r = g.binop(o, v[0], v[1], Type::I32);
                                g.boxed_bool(r)
                            }),
                            |g| g.compare(kind, a, b),
                        )
                    },
                );
            }
            Not => {
                let v = self.slot(d - 1);
                let (fast, truth) = self.truthy_inline(v);
                self.fast_path(
                    fast,
                    |g, _| {
                        let n = g.unop(Operator::I32Eqz, truth, Type::I32);
                        let r = g.boxed_bool(n);
                        g.set_slot(d - 1, r);
                    },
                    |g| {
                        let t = g.truthy(v);
                        let n = g.unop(Operator::I32Eqz, t, Type::I32);
                        let r = g.boxed_bool(n);
                        g.set_slot(d - 1, r);
                    },
                );
            }
            Typeof | TypeofExpr => {
                let v = self.slot(d - 1);
                let r = self
                    .call(h.typeof_, &[self.cx, v], Some(Type::I64))
                    .unwrap();
                self.set_slot(d - 1, r);
            }
            TypeofEq => {
                let operand = p.next_uint8().unwrap();
                let v = self.slot(d - 1);
                let o = self.i32c(u32::from(operand));
                let b = self
                    .call(h.typeof_eq, &[self.cx, v, o], Some(Type::I32))
                    .unwrap();
                let r = self.boxed_bool(b);
                self.set_slot(d - 1, r);
            }
            StrictConstantEq | StrictConstantNe => {
                let operand = p.next_uint16().unwrap();
                let v = self.slot(d - 1);
                let mut b = self.constant_strict_eq(v, operand);
                if op == StrictConstantNe {
                    let z = self.i32c(0);
                    b = self.binop(Operator::I32Eq, b, z, Type::I32);
                }
                let r = self.boxed_bool(b);
                self.set_slot(d - 1, r);
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
}
