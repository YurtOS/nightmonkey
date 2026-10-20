//! numeric: methods of `Lower`, split out of `lower.rs` unchanged.

use super::*;

impl<'a> Lower<'a> {
    /// An equality's inline arms (against a string literal, then the
    /// tag ladder), taking edge 0.
    pub(super) fn equality_arms(&mut self, inst: mir::Inst, cc: JsCc, a: &[Value]) -> R<()> {
        if matches!(cc, JsCc::Eq | JsCc::Ne | JsCc::StrictEq | JsCc::StrictNe) {
            // Against a string literal (an atom): `literal_eq`'s ladder.
            let args = self.f.insts[inst].args.clone();
            if self.is_str_literal(args[1]) {
                self.literal_eq(inst, cc, a[0], a[1])?;
            } else if self.is_str_literal(args[0]) {
                self.literal_eq(inst, cc, a[1], a[0])?;
            }
            self.equality_fast_arm(inst, cc, a[0], a[1])?;
        }
        Ok(())
    }

    /// The inline arms of a generic numeric op, as baseline's (and the
    /// portable baseline interpreter's) are: all-int32 operands, then all
    /// numbers as doubles, each taking `ok_clean` with the boxed result (a
    /// bool for a compare); otherwise falls through to the helper. Without
    /// them MIR would run code whose types it does not know slower than
    /// baseline does. Emits nothing for an op with no arm.
    pub(super) fn numeric_fast_arms(&mut self, inst: mir::Inst, op: &Opcode, a: &[Value]) -> R<()> {
        use Operator as O;
        #[derive(Clone, Copy)]
        enum Int {
            Checked(O, O),
            Mul,
            Plain(O),
            Mod,
            Ursh,
            Step(O, u32),
            Neg,
            BitNot,
            Same,
            Cmp(O),
        }
        #[derive(Clone, Copy)]
        enum Num {
            Bin(O),
            Step(O),
            Neg,
            Same,
            Cmp(O),
        }
        let (n, int, num): (usize, Option<Int>, Option<Num>) = match *op {
            Opcode::JsAdd => (
                2,
                Some(Int::Checked(O::I32Add, O::I64Add)),
                Some(Num::Bin(O::F64Add)),
            ),
            Opcode::JsBinop(b) => match b {
                JsBinop::Sub => (
                    2,
                    Some(Int::Checked(O::I32Sub, O::I64Sub)),
                    Some(Num::Bin(O::F64Sub)),
                ),
                JsBinop::Mul => (2, Some(Int::Mul), Some(Num::Bin(O::F64Mul))),
                JsBinop::Div => (2, None, Some(Num::Bin(O::F64Div))),
                // Double `%` is fmod, which Wasm lacks.
                JsBinop::Mod => (2, Some(Int::Mod), None),
                JsBinop::BitAnd => (2, Some(Int::Plain(O::I32And)), None),
                JsBinop::BitOr => (2, Some(Int::Plain(O::I32Or)), None),
                JsBinop::BitXor => (2, Some(Int::Plain(O::I32Xor)), None),
                // Wasm masks shift counts to 5 bits, as JS does.
                JsBinop::Lsh => (2, Some(Int::Plain(O::I32Shl)), None),
                JsBinop::Rsh => (2, Some(Int::Plain(O::I32ShrS)), None),
                JsBinop::Ursh => (2, Some(Int::Ursh), None),
                JsBinop::Pow => return Ok(()),
            },
            Opcode::JsUnop(u) => match u {
                JsUnop::Inc => (
                    1,
                    Some(Int::Step(O::I32Add, i32::MAX as u32)),
                    Some(Num::Step(O::F64Add)),
                ),
                JsUnop::Dec => (
                    1,
                    Some(Int::Step(O::I32Sub, i32::MIN as u32)),
                    Some(Num::Step(O::F64Sub)),
                ),
                JsUnop::Neg => (1, Some(Int::Neg), Some(Num::Neg)),
                JsUnop::BitNot => (1, Some(Int::BitNot), None),
                JsUnop::Pos => (1, Some(Int::Same), Some(Num::Same)),
            },
            Opcode::JsToNumeric => (1, Some(Int::Same), Some(Num::Same)),
            Opcode::JsCompare(cc) => {
                let (i, f) = match cc {
                    JsCc::Lt => (O::I32LtS, O::F64Lt),
                    JsCc::Le => (O::I32LeS, O::F64Le),
                    JsCc::Gt => (O::I32GtS, O::F64Gt),
                    JsCc::Ge => (O::I32GeS, O::F64Ge),
                    _ => return Ok(()),
                };
                (2, Some(Int::Cmp(i)), Some(Num::Cmp(f)))
            }
            _ => return Ok(()),
        };
        let ops = &a[..n];
        let slow = self.body.add_block();
        let num_b = if num.is_some() {
            self.body.add_block()
        } else {
            slow
        };
        if let Some(int) = int {
            let mut all = self.i32c(1);
            for &v in ops {
                let t = self.tag_of(v);
                let is = self.tag_is(t, TAG_INT32 as u32);
                all = self.bin(O::I32And, all, is, Type::I32);
            }
            let int_b = self.body.add_block();
            self.cond_br(all, Self::to(int_b), Self::to(num_b));
            self.cur = int_b;
            let x = self.un(O::I32WrapI64, ops[0], Type::I32);
            let y = if n == 2 {
                self.un(O::I32WrapI64, ops[1], Type::I32)
            } else {
                x
            };
            let one = self.i32c(1);
            let (r, ok) = match int {
                Int::Checked(o, wide) => {
                    let r = self.bin(o, x, y, Type::I32);
                    let x64 = self.un(O::I64ExtendI32S, x, Type::I64);
                    let y64 = self.un(O::I64ExtendI32S, y, Type::I64);
                    let w = self.bin(wide, x64, y64, Type::I64);
                    let r64 = self.un(O::I64ExtendI32S, r, Type::I64);
                    (r, self.bin(O::I64Eq, w, r64, Type::I32))
                }
                Int::Mul => {
                    let x64 = self.un(O::I64ExtendI32S, x, Type::I64);
                    let y64 = self.un(O::I64ExtendI32S, y, Type::I64);
                    let w = self.bin(O::I64Mul, x64, y64, Type::I64);
                    let r = self.un(O::I32WrapI64, w, Type::I32);
                    let r64 = self.un(O::I64ExtendI32S, r, Type::I64);
                    let fits = self.bin(O::I64Eq, w, r64, Type::I32);
                    // A zero product with a negative operand is -0.
                    let zero = self.un(O::I32Eqz, r, Type::I32);
                    let xy = self.bin(O::I32Or, x, y, Type::I32);
                    let z = self.i32c(0);
                    let neg = self.bin(O::I32LtS, xy, z, Type::I32);
                    let negz = self.bin(O::I32And, zero, neg, Type::I32);
                    let not_negz = self.un(O::I32Eqz, negz, Type::I32);
                    (r, self.bin(O::I32And, fits, not_negz, Type::I32))
                }
                Int::Plain(o) => (self.bin(o, x, y, Type::I32), one),
                // A non-negative dividend and a positive divisor: the
                // result is the unsigned remainder, never -0.
                Int::Mod => {
                    let z = self.i32c(0);
                    let xn = self.bin(O::I32GeS, x, z, Type::I32);
                    let yp = self.bin(O::I32GtS, y, z, Type::I32);
                    let ok = self.bin(O::I32And, xn, yp, Type::I32);
                    // The divisor is forced to 1 off the arm, so the
                    // remainder never traps.
                    let d = self.select(Type::I32, y, one, yp);
                    (self.bin(O::I32RemU, x, d, Type::I32), ok)
                }
                // Only a result below 2^31 is an int32.
                Int::Ursh => {
                    let r = self.bin(O::I32ShrU, x, y, Type::I32);
                    let z = self.i32c(0);
                    (r, self.bin(O::I32GeS, r, z, Type::I32))
                }
                Int::Step(o, limit) => {
                    let l = self.i32c(limit);
                    let ok = self.bin(O::I32Ne, x, l, Type::I32);
                    (self.bin(o, x, one, Type::I32), ok)
                }
                // Neg of 0 is -0 and of INT32_MIN overflows.
                Int::Neg => {
                    let z = self.i32c(0);
                    let min = self.i32c(i32::MIN as u32);
                    let nz = self.bin(O::I32Ne, x, z, Type::I32);
                    let nm = self.bin(O::I32Ne, x, min, Type::I32);
                    let ok = self.bin(O::I32And, nz, nm, Type::I32);
                    (self.bin(O::I32Sub, z, x, Type::I32), ok)
                }
                Int::BitNot => {
                    let m1 = self.i32c(u32::MAX);
                    (self.bin(O::I32Xor, x, m1, Type::I32), one)
                }
                Int::Same => (x, one),
                Int::Cmp(o) => (self.bin(o, x, y, Type::I32), one),
            };
            let out = if matches!(int, Int::Cmp(_)) {
                r
            } else {
                self.box_tagged(TAG_INT32, r)
            };
            let t = self.edge(inst, 0, &[out])?;
            self.cond_br(ok, t, Self::to(num_b));
        } else {
            self.terminate(Terminator::Br {
                target: Self::to(num_b),
            });
        }
        if let Some(num) = num {
            self.cur = num_b;
            let mut all = self.i32c(1);
            for &v in ops {
                let t = self.tag_of(v);
                let int = self.tag_is(t, TAG_INT32 as u32);
                let clear = self.i32c(TAG_CLEAR);
                let dbl = self.bin(O::I32LtU, t, clear, Type::I32);
                let is = self.bin(O::I32Or, int, dbl, Type::I32);
                all = self.bin(O::I32And, all, is, Type::I32);
            }
            let go = self.body.add_block();
            self.cond_br(all, Self::to(go), Self::to(slow));
            self.cur = go;
            let x = self.to_f64(ops[0]);
            let y = if n == 2 { self.to_f64(ops[1]) } else { x };
            let out = match num {
                Num::Bin(o) => {
                    let r = self.bin(o, x, y, Type::F64);
                    self.box_number(r)
                }
                Num::Step(o) => {
                    let one = self.f64c(1f64.to_bits());
                    let r = self.bin(o, x, one, Type::F64);
                    self.box_number(r)
                }
                Num::Neg => {
                    let r = self.un(O::F64Neg, x, Type::F64);
                    self.box_number(r)
                }
                Num::Same => ops[0],
                Num::Cmp(o) => self.bin(o, x, y, Type::I32),
            };
            let t = self.edge(inst, 0, &[out])?;
            self.terminate(Terminator::Br { target: t });
        }
        self.cur = slow;
        Ok(())
    }

    /// Whether an object that emulates `undefined` (`document.all`) may
    /// exist: the runtime's fuse word, nonzero once one's class is seen.
    pub(super) fn dda_possible(&mut self) -> Value {
        let slot = self.i32c(self.h.dda_fuse_addr_slot);
        let addr = self.load_i32(slot, 0);
        self.load_i32(addr, 0)
    }

    /// ToBoolean of boxed `v`, inline for int32, boolean, null, undefined
    /// and (while no object emulates `undefined`) objects; the leaf helper
    /// otherwise.
    pub(super) fn to_bool(&mut self, v: Value) -> Value {
        let join = self.body.add_block();
        let r = self.body.add_blockparam(join, Type::I32);
        let tag = self.tag_of(v);
        let low = self.un(Operator::I32WrapI64, v, Type::I32);
        // int32 and boolean: the payload.
        let int = self.tag_is(tag, TAG_INT32 as u32);
        let boolean = self.tag_is(tag, TAG_BOOLEAN as u32);
        let payload = self.bin(Operator::I32Or, int, boolean, Type::I32);
        let nz = self.i32c(0);
        let low_t = self.bin(Operator::I32Ne, low, nz, Type::I32);
        let next = self.body.add_block();
        self.cond_br(
            payload,
            BlockTarget {
                block: join,
                args: vec![low_t],
            },
            Self::to(next),
        );
        self.cur = next;
        let null = self.tag_is(tag, TAG_NULL as u32);
        let undef = self.tag_is(tag, TAG_UNDEFINED as u32);
        let nullish = self.bin(Operator::I32Or, null, undef, Type::I32);
        let zero = self.i32c(0);
        let next = self.body.add_block();
        self.cond_br(
            nullish,
            BlockTarget {
                block: join,
                args: vec![zero],
            },
            Self::to(next),
        );
        self.cur = next;
        let obj = self.tag_is(tag, TAG_OBJECT as u32);
        let (obj_blk, slow) = (self.body.add_block(), self.body.add_block());
        self.cond_br(obj, Self::to(obj_blk), Self::to(slow));
        self.cur = obj_blk;
        let dda = self.dda_possible();
        let one = self.i32c(1);
        self.cond_br(
            dda,
            Self::to(slow),
            BlockTarget {
                block: join,
                args: vec![one],
            },
        );
        self.cur = slow;
        // A leaf: no GC, no JS.
        let t = self.call1(self.h.to_boolean, &[self.cx, v], Type::I32);
        self.terminate(Terminator::Br {
            target: BlockTarget {
                block: join,
                args: vec![t],
            },
        });
        self.cur = join;
        r
    }

    /// The inline arm of an equality compare, taking `ok_clean` with the
    /// result where the operands decide it by their bits: strictly, when
    /// neither is a double, string or BigInt (so equal values have equal
    /// bits); loosely, when both are int32s or both booleans, or both are
    /// null, undefined or an object while no object emulates `undefined`
    /// (then null and undefined are equal to each other and nothing else,
    /// and objects by identity). Otherwise falls through to the helper.
    pub(super) fn equality_fast_arm(
        &mut self,
        inst: mir::Inst,
        cc: JsCc,
        a: Value,
        b: Value,
    ) -> R<()> {
        let (ta, tb) = (self.tag_of(a), self.tag_of(b));
        let bits_eq = self.bin(Operator::I64Eq, a, b, Type::I32);
        let fast = self.body.add_block();
        let slow = self.body.add_block();
        let r = match cc {
            JsCc::StrictEq | JsCc::StrictNe => {
                // int32, boolean, undefined, null (consecutive tags), symbol
                // or object.
                let simple = |l: &mut Self, t: Value| {
                    let lo = l.i32c(TAG_INT32 as u32);
                    let rel = l.bin(Operator::I32Sub, t, lo, Type::I32);
                    let three = l.i32c(3);
                    let prim = l.bin(Operator::I32LeU, rel, three, Type::I32);
                    let sym = l.tag_is(t, TAG_SYMBOL as u32);
                    let obj = l.tag_is(t, TAG_OBJECT as u32);
                    let x = l.bin(Operator::I32Or, prim, sym, Type::I32);
                    l.bin(Operator::I32Or, x, obj, Type::I32)
                };
                let (sa, sb) = (simple(self, ta), simple(self, tb));
                let both = self.bin(Operator::I32And, sa, sb, Type::I32);
                self.cond_br(both, Self::to(fast), Self::to(slow));
                self.cur = fast;
                bits_eq
            }
            _ => {
                let same = self.bin(Operator::I32Eq, ta, tb, Type::I32);
                let int = self.tag_is(ta, TAG_INT32 as u32);
                let boolean = self.tag_is(ta, TAG_BOOLEAN as u32);
                let ib = self.bin(Operator::I32Or, int, boolean, Type::I32);
                let same_ib = self.bin(Operator::I32And, same, ib, Type::I32);
                let nullish = |l: &mut Self, t: Value| {
                    let null = l.tag_is(t, TAG_NULL as u32);
                    let undef = l.tag_is(t, TAG_UNDEFINED as u32);
                    l.bin(Operator::I32Or, null, undef, Type::I32)
                };
                let (na, nb) = (nullish(self, ta), nullish(self, tb));
                let oa = self.tag_is(ta, TAG_OBJECT as u32);
                let ob = self.tag_is(tb, TAG_OBJECT as u32);
                let ka = self.bin(Operator::I32Or, na, oa, Type::I32);
                let kb = self.bin(Operator::I32Or, nb, ob, Type::I32);
                let both_k = self.bin(Operator::I32And, ka, kb, Type::I32);
                let (ib_blk, k_blk) = (self.body.add_block(), self.body.add_block());
                self.cond_br(same_ib, Self::to(ib_blk), Self::to(k_blk));
                self.cur = ib_blk;
                self.terminate(Terminator::Br {
                    target: Self::to(fast),
                });
                self.cur = k_blk;
                let dda_blk = self.body.add_block();
                self.cond_br(both_k, Self::to(dda_blk), Self::to(slow));
                self.cur = dda_blk;
                let dda = self.dda_possible();
                self.cond_br(dda, Self::to(slow), Self::to(fast));
                self.cur = fast;
                let both_n = self.bin(Operator::I32And, na, nb, Type::I32);
                self.bin(Operator::I32Or, bits_eq, both_n, Type::I32)
            }
        };
        let r = if matches!(cc, JsCc::Ne | JsCc::StrictNe) {
            self.un(Operator::I32Eqz, r, Type::I32)
        } else {
            r
        };
        let t = self.edge(inst, 0, &[r])?;
        self.terminate(Terminator::Br { target: t });
        self.cur = slow;
        {
            // Two strings: one pointer is
            // equal; different lengths (a rope carries its length, so no
            // flatten) or two atoms (deduplicated) are unequal. Else the
            // helper compares the characters.
            use crate::wasm::mir::abi::{STRING_ATOM_BIT, STRING_FLAGS_OFFSET};
            let generic = self.body.add_block();
            let sa = self.tag_is(ta, TAG_STRING as u32);
            let sb = self.tag_is(tb, TAG_STRING as u32);
            let both = self.bin(Operator::I32And, sa, sb, Type::I32);
            self.check(both, generic);
            let negate = matches!(cc, JsCc::Ne | JsCc::StrictNe);
            let (eq_b, ne_b, len_b) = (
                self.body.add_block(),
                self.body.add_block(),
                self.body.add_block(),
            );
            self.cond_br(bits_eq, Self::to(eq_b), Self::to(len_b));
            self.cur = len_b;
            let (ap, bp) = (
                self.un(Operator::I32WrapI64, a, Type::I32),
                self.un(Operator::I32WrapI64, b, Type::I32),
            );
            let al = self.load_i32(ap, STRING_LENGTH_OFFSET);
            let bl = self.load_i32(bp, STRING_LENGTH_OFFSET);
            let len_eq = self.bin(Operator::I32Eq, al, bl, Type::I32);
            let atom_b = self.body.add_block();
            self.cond_br(len_eq, Self::to(atom_b), Self::to(ne_b));
            self.cur = atom_b;
            let af = self.load_i32(ap, STRING_FLAGS_OFFSET);
            let bf = self.load_i32(bp, STRING_FLAGS_OFFSET);
            let flags = self.bin(Operator::I32And, af, bf, Type::I32);
            let atom = self.i32c(STRING_ATOM_BIT);
            let both_atom = self.bin(Operator::I32And, flags, atom, Type::I32);
            self.cond_br(both_atom, Self::to(ne_b), Self::to(generic));
            for (blk, equal) in [(eq_b, true), (ne_b, false)] {
                self.cur = blk;
                let v = self.i32c(u32::from(equal != negate));
                let t = self.edge(inst, 0, &[v])?;
                self.terminate(Terminator::Br { target: t });
            }
            self.cur = generic;
        }
        Ok(())
    }

    /// Native `Math` calls on numbers, matched by the callee's
    /// `JSNative` against the pristine natives table, so a replaced or
    /// self-hosted clone is told apart without fuses: one-argument
    /// sqrt/abs/floor/ceil/trunc/fround inline, sin/cos by a leaf, clz32;
    /// two-argument min/max inline, pow by a leaf, imul. The result takes
    /// `ok_clean`; anything else goes to `other`.
    pub(super) fn math_arms(
        &mut self,
        inst: mir::Inst,
        ops: &[Value],
        native: Value,
        other: Block,
    ) -> R<()> {
        use crate::wasm::translate::{
            MN_ABS, MN_CEIL, MN_CLZ32, MN_COS, MN_FLOOR, MN_FROUND, MN_IMUL, MN_MAX, MN_MIN,
            MN_POW, MN_SIN, MN_SQRT, MN_TRUNC,
        };
        self.check(native, other);
        let cp = self.un(Operator::I32WrapI64, ops[0], Type::I32);
        let nf = self.load_i32(cp, FUNC_ENV_SLOT_OFFSET);
        let is = |s: &mut Self, idx: u32| {
            let c = s.i32c(s.h.math_natives_base + 4 * idx);
            let slot = s.load_i32(c, 0);
            s.bin(Operator::I32Eq, nf, slot, Type::I32)
        };
        let is_num = |s: &mut Self, v: Value| {
            let t = s.tag_of(v);
            let k = s.i32c(TAG_INT32 as u32);
            s.bin(Operator::I32LeU, t, k, Type::I32)
        };
        let done = self.body.add_block();
        let res = self.body.add_blockparam(done, Type::F64);
        if ops.len() == 3 {
            let x = ops[2];
            let num = is_num(self, x);
            self.check(num, other);
            let f = self.to_f64(x);
            let (sqrt, abs, floor, ceil) = (
                is(self, MN_SQRT),
                is(self, MN_ABS),
                is(self, MN_FLOOR),
                is(self, MN_CEIL),
            );
            let (trunc, fround, sin, cos) = (
                is(self, MN_TRUNC),
                is(self, MN_FROUND),
                is(self, MN_SIN),
                is(self, MN_COS),
            );
            let clz = is(self, MN_CLZ32);
            // clz32: ToUint32 then i32.clz (|f| < 2^63 truncates exactly).
            let (clz_b, rest) = (self.body.add_block(), self.body.add_block());
            self.cond_br(clz, Self::to(clz_b), Self::to(rest));
            self.cur = clz_b;
            let af = self.un(Operator::F64Abs, f, Type::F64);
            let lim = self.f64c(9223372036854775808.0f64.to_bits());
            let ok = self.bin(Operator::F64Lt, af, lim, Type::I32);
            self.check(ok, other);
            let i = self.un(Operator::I64TruncSatF64S, f, Type::I64);
            let i = self.un(Operator::I32WrapI64, i, Type::I32);
            let c = self.un(Operator::I32Clz, i, Type::I32);
            let r = self.box_tagged(TAG_INT32, c);
            let t = self.edge(inst, 0, &[r])?;
            self.terminate(Terminator::Br { target: t });
            self.cur = rest;
            let trig = self.bin(Operator::I32Or, sin, cos, Type::I32);
            let (trig_b, opc) = (self.body.add_block(), self.body.add_block());
            self.cond_br(trig, Self::to(trig_b), Self::to(opc));
            self.cur = trig_b;
            let r = self.call1(self.h.math_unary, &[cos, f], Type::F64);
            self.terminate(Terminator::Br {
                target: BlockTarget {
                    block: done,
                    args: vec![r],
                },
            });
            self.cur = opc;
            let a = self.bin(Operator::I32Or, sqrt, abs, Type::I32);
            let b = self.bin(Operator::I32Or, floor, ceil, Type::I32);
            let c = self.bin(Operator::I32Or, trunc, fround, Type::I32);
            let any = self.bin(Operator::I32Or, a, b, Type::I32);
            let any = self.bin(Operator::I32Or, any, c, Type::I32);
            self.check(any, other);
            let r_sqrt = self.un(Operator::F64Sqrt, f, Type::F64);
            let r_abs = self.un(Operator::F64Abs, f, Type::F64);
            let r_floor = self.un(Operator::F64Floor, f, Type::F64);
            let r_ceil = self.un(Operator::F64Ceil, f, Type::F64);
            let r_trunc = self.un(Operator::F64Trunc, f, Type::F64);
            let f32v = self.un(Operator::F32DemoteF64, f, Type::F32);
            let r_fround = self.un(Operator::F64PromoteF32, f32v, Type::F64);
            let sel = self.select(Type::F64, r_trunc, r_fround, trunc);
            let sel = self.select(Type::F64, r_ceil, sel, ceil);
            let sel = self.select(Type::F64, r_floor, sel, floor);
            let sel = self.select(Type::F64, r_abs, sel, abs);
            let r = self.select(Type::F64, r_sqrt, sel, sqrt);
            self.terminate(Terminator::Br {
                target: BlockTarget {
                    block: done,
                    args: vec![r],
                },
            });
        } else {
            let (x, y) = (ops[2], ops[3]);
            let nx = is_num(self, x);
            let ny = is_num(self, y);
            let both = self.bin(Operator::I32And, nx, ny, Type::I32);
            self.check(both, other);
            let (fx, fy) = (self.to_f64(x), self.to_f64(y));
            let (min, max, pow, imul) = (
                is(self, MN_MIN),
                is(self, MN_MAX),
                is(self, MN_POW),
                is(self, MN_IMUL),
            );
            let (imul_b, rest) = (self.body.add_block(), self.body.add_block());
            self.cond_br(imul, Self::to(imul_b), Self::to(rest));
            // imul: ToInt32 of each (|f| < 2^63 truncates exactly), i32.mul.
            self.cur = imul_b;
            let lim = self.f64c(9223372036854775808.0f64.to_bits());
            let ax = self.un(Operator::F64Abs, fx, Type::F64);
            let ay = self.un(Operator::F64Abs, fy, Type::F64);
            let sx = self.bin(Operator::F64Lt, ax, lim, Type::I32);
            let sy = self.bin(Operator::F64Lt, ay, lim, Type::I32);
            let safe = self.bin(Operator::I32And, sx, sy, Type::I32);
            self.check(safe, other);
            let ix = self.un(Operator::I64TruncSatF64S, fx, Type::I64);
            let ix = self.un(Operator::I32WrapI64, ix, Type::I32);
            let iy = self.un(Operator::I64TruncSatF64S, fy, Type::I64);
            let iy = self.un(Operator::I32WrapI64, iy, Type::I32);
            let m = self.bin(Operator::I32Mul, ix, iy, Type::I32);
            let r = self.box_tagged(TAG_INT32, m);
            let t = self.edge(inst, 0, &[r])?;
            self.terminate(Terminator::Br { target: t });
            self.cur = rest;
            let (pow_b, mm) = (self.body.add_block(), self.body.add_block());
            self.cond_br(pow, Self::to(pow_b), Self::to(mm));
            self.cur = pow_b;
            let r = self.call1(self.h.math_pow, &[fx, fy], Type::F64);
            self.terminate(Terminator::Br {
                target: BlockTarget {
                    block: done,
                    args: vec![r],
                },
            });
            self.cur = mm;
            let any = self.bin(Operator::I32Or, min, max, Type::I32);
            self.check(any, other);
            let r_min = self.bin(Operator::F64Min, fx, fy, Type::F64);
            let r_max = self.bin(Operator::F64Max, fx, fy, Type::F64);
            let r = self.select(Type::F64, r_min, r_max, min);
            self.terminate(Terminator::Br {
                target: BlockTarget {
                    block: done,
                    args: vec![r],
                },
            });
        }
        self.cur = done;
        let b = self.box_number(res);
        let t = self.edge(inst, 0, &[b])?;
        self.terminate(Terminator::Br { target: t });
        Ok(())
    }
}
