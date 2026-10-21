//! numeric: methods of `Gen`, split out of `codegen.rs` unchanged.

use super::*;

impl<'a> Gen<'a> {
    /// `v` to a raw i32 truth value.
    pub(super) fn truthy(&mut self, v: Value) -> Value {
        let tb = self.h.to_boolean;
        self.call(tb, &[self.cx, v], Some(Type::I32)).unwrap()
    }

    /// Whether `v` is a number: an int32, or a double (every high word
    /// below `TAG_CLEAR`).
    pub(super) fn is_number(&mut self, v: Value) -> Value {
        let t = self.tag_of(v);
        let k = self.i32c(TAG_INT32 as u32);
        self.binop(Operator::I32LeU, t, k, Type::I32)
    }

    /// A numeric op's inline arms, in order: `int` on the int32 payloads
    /// when every operand is an int32 (it may bail to the next arm), `num`
    /// on the operands as doubles when every operand is a number, else
    /// `slow`, the op's helper call. The arms return the boxed result,
    /// stored over the operands. Each arm is optional.
    pub(super) fn number_arms(
        &mut self,
        ops: &[Value],
        int: Option<&IntArm<'_, 'a>>,
        num: Option<&NumArm<'_, 'a>>,
        slow: impl FnOnce(&mut Self),
    ) {
        let k = self.d - u32::try_from(ops.len()).unwrap();
        let join = self.body.add_block();
        let slow_b = self.body.add_block();
        let num_b = if num.is_some() {
            self.body.add_block()
        } else {
            slow_b
        };
        if let Some(int) = int {
            let mut all = None;
            for &v in ops {
                let t = self.tag_eq(v, TAG_INT32);
                all = Some(match all {
                    Some(a) => self.binop(Operator::I32And, a, t, Type::I32),
                    None => t,
                });
            }
            let int_b = self.body.add_block();
            self.cond_br(all.unwrap(), Self::goto(int_b), Self::goto(num_b));
            (self.cur, self.live) = (int_b, true);
            let raw: Vec<Value> = ops
                .iter()
                .map(|&v| self.unop(Operator::I32WrapI64, v, Type::I32))
                .collect();
            let r = int(self, &raw, num_b);
            self.set_slot(k, r);
            self.br(Self::goto(join));
        } else {
            self.br(Self::goto(num_b));
        }
        if let Some(num) = num {
            (self.cur, self.live) = (num_b, true);
            let mut all = None;
            for &v in ops {
                let t = self.is_number(v);
                all = Some(match all {
                    Some(a) => self.binop(Operator::I32And, a, t, Type::I32),
                    None => t,
                });
            }
            self.bail_unless(all.unwrap(), slow_b);
            let fs: Vec<Value> = ops.iter().map(|&v| self.to_f64(v)).collect();
            let r = num(self, &fs);
            self.set_slot(k, r);
            self.br(Self::goto(join));
        }
        (self.cur, self.live) = (slow_b, true);
        slow(self);
        if self.live {
            self.br(Self::goto(join));
        }
        (self.cur, self.live) = (join, true);
    }

    /// `x === k` for a `StrictConstantEq` operand `k` (the high byte its
    /// type: 2 boolean, 3 undefined, 4 null, else an int8), as a raw bool:
    /// `ConstantStrictEqual` inline.
    pub(super) fn constant_strict_eq(&mut self, x: Value, k: u16) -> Value {
        let (ty, lo) = ((k >> 8) & 0xFF, (k & 0xFF) as u8);
        match ty {
            3 => self.tag_eq(x, TAG_UNDEFINED),
            4 => self.tag_eq(x, TAG_NULL),
            2 => {
                let want = self.i64c((TAG_BOOLEAN << 32) | u64::from(lo & 1));
                self.binop(Operator::I64Eq, x, want, Type::I32)
            }
            _ => {
                // An int32 of that value, or a double equal to it.
                let n = i32::from(lo as i8);
                let tag = self.tag_of(x);
                let it = self.i32c(TAG_INT32 as u32);
                let is_int = self.binop(Operator::I32Eq, tag, it, Type::I32);
                let low = self.unop(Operator::I32WrapI64, x, Type::I32);
                let nv = self.i32c(n as u32);
                let low_eq = self.binop(Operator::I32Eq, low, nv, Type::I32);
                let int_eq = self.binop(Operator::I32And, is_int, low_eq, Type::I32);
                let is_dbl = self.binop(Operator::I32LtU, tag, it, Type::I32);
                let d = self.unop(Operator::F64ReinterpretI64, x, Type::F64);
                let dv = self.f64c(f64::from(n));
                let dbl_eq = self.binop(Operator::F64Eq, d, dv, Type::I32);
                let dbl_eq = self.binop(Operator::I32And, is_dbl, dbl_eq, Type::I32);
                self.binop(Operator::I32Or, int_eq, dbl_eq, Type::I32)
            }
        }
    }

    /// A number (int32 or double) as an f64.
    pub(super) fn to_f64(&mut self, v: Value) -> Value {
        let is_int = self.tag_eq(v, TAG_INT32);
        let low = self.unop(Operator::I32WrapI64, v, Type::I32);
        let fi = self.unop(Operator::F64ConvertI32S, low, Type::F64);
        let fd = self.unop(Operator::F64ReinterpretI64, v, Type::F64);
        self.select(Type::F64, fi, fd, is_int)
    }

    /// Box an f64 as the engine's `NumberValue` does: an int32 when it is
    /// one exactly (not -0), else a double, with NaN canonicalized (a
    /// non-canonical NaN could alias a boxed tag).
    /// Rewrite the Value at `base + off` as `box_number` boxes it.
    pub(super) fn canon_slot(&mut self, base: Value, off: u32) {
        let v = self.load_i64(base, off);
        let tag = self.tag_of(v);
        let k = self.i32c(TAG_CLEAR);
        let is_dbl = self.binop(Operator::I32LtU, tag, k, Type::I32);
        let f = self.unop(Operator::F64ReinterpretI64, v, Type::F64);
        let b = self.box_number(f);
        let nv = self.select(Type::I64, b, v, is_dbl);
        self.store_i64(base, off, nv);
    }

    pub(super) fn box_number(&mut self, x: Value) -> Value {
        let i = self.unop(Operator::I32TruncSatF64S, x, Type::I32);
        let back = self.unop(Operator::F64ConvertI32S, i, Type::F64);
        let exact = self.binop(Operator::F64Eq, back, x, Type::I32);
        let bits = self.unop(Operator::I64ReinterpretF64, x, Type::I64);
        let negz = self.i64c(1 << 63);
        let not_negz = self.binop(Operator::I64Ne, bits, negz, Type::I32);
        let is_int = self.binop(Operator::I32And, exact, not_negz, Type::I32);
        let nan = self.binop(Operator::F64Ne, x, x, Type::I32);
        let canon = self.i64c(CANONICAL_NAN_BITS);
        let dbl = self.select(Type::I64, canon, bits, nan);
        let int = self.boxed_int32(i);
        self.select(Type::I64, int, dbl, is_int)
    }

    /// Whether the sign bit of `v` is set.
    pub(super) fn is_neg(&mut self, v: Value) -> Value {
        let z = self.i32c(0);
        self.binop(Operator::I32LtS, v, z, Type::I32)
    }

    pub(super) fn add_i32(&mut self, x: Value, y: Value, slow: Block) -> Value {
        let r = self.binop(Operator::I32Add, x, y, Type::I32);
        // Signed overflow: both operands' signs differ from the result's.
        let xr = self.binop(Operator::I32Xor, x, r, Type::I32);
        let yr = self.binop(Operator::I32Xor, y, r, Type::I32);
        let both = self.binop(Operator::I32And, xr, yr, Type::I32);
        let ovf = self.is_neg(both);
        self.bail_if(ovf, slow);
        r
    }

    pub(super) fn sub_i32(&mut self, x: Value, y: Value, slow: Block) -> Value {
        let r = self.binop(Operator::I32Sub, x, y, Type::I32);
        // Signed overflow: the operands' signs differ, and the result's
        // differs from x's.
        let xy = self.binop(Operator::I32Xor, x, y, Type::I32);
        let xr = self.binop(Operator::I32Xor, x, r, Type::I32);
        let both = self.binop(Operator::I32And, xy, xr, Type::I32);
        let ovf = self.is_neg(both);
        self.bail_if(ovf, slow);
        r
    }

    pub(super) fn mul_i32(&mut self, x: Value, y: Value, slow: Block) -> Value {
        let x64 = self.unop(Operator::I64ExtendI32S, x, Type::I64);
        let y64 = self.unop(Operator::I64ExtendI32S, y, Type::I64);
        let p = self.binop(Operator::I64Mul, x64, y64, Type::I64);
        let r = self.unop(Operator::I32WrapI64, p, Type::I32);
        let r64 = self.unop(Operator::I64ExtendI32S, r, Type::I64);
        let ovf = self.binop(Operator::I64Ne, p, r64, Type::I32);
        self.bail_if(ovf, slow);
        // A zero product with a negative operand is -0, a double.
        let z = self.i32c(0);
        let is_zero = self.binop(Operator::I32Eq, r, z, Type::I32);
        let xy = self.binop(Operator::I32Or, x, y, Type::I32);
        let neg = self.is_neg(xy);
        let neg_zero = self.binop(Operator::I32And, is_zero, neg, Type::I32);
        self.bail_if(neg_zero, slow);
        r
    }

    /// `x % y` for a non-negative `x` and a positive `y`: an int32, never
    /// -0 (the other signs are left to the next arm).
    pub(super) fn mod_i32(&mut self, x: Value, y: Value, slow: Block) -> Value {
        let xn = self.is_neg(x);
        let z = self.i32c(0);
        let yp = self.binop(Operator::I32GtS, y, z, Type::I32);
        let bad = self.unop(Operator::I32Eqz, yp, Type::I32);
        let bad = self.binop(Operator::I32Or, xn, bad, Type::I32);
        self.bail_if(bad, slow);
        self.binop(Operator::I32RemS, x, y, Type::I32)
    }

    pub(super) fn ursh_i32(&mut self, x: Value, y: Value, slow: Block) -> Value {
        let r = self.binop(Operator::I32ShrU, x, y, Type::I32);
        // A result of 2^31 or more is not an int32.
        let big = self.is_neg(r);
        self.bail_if(big, slow);
        r
    }

    /// ToBoolean's inline cases, decided from the tag: int32, boolean,
    /// undefined and null (the contiguous tags `TAG_INT32..=TAG_NULL`), and
    /// doubles. Returns (whether `v` is one of them, its truth value if so).
    pub(super) fn truthy_inline(&mut self, v: Value) -> (Value, Value) {
        const _: () = assert!(
            TAG_BOOLEAN == TAG_INT32 + 1
                && TAG_UNDEFINED == TAG_INT32 + 2
                && TAG_NULL == TAG_INT32 + 3
        );
        let t = self.tag_of(v);
        let base = self.i32c(TAG_INT32 as u32);
        let rel = self.binop(Operator::I32Sub, t, base, Type::I32);
        let four = self.i32c(4);
        let tagged = self.binop(Operator::I32LtU, rel, four, Type::I32);
        // Int32 and boolean: a nonzero payload. Undefined and null: false.
        let two = self.i32c(2);
        let has_payload = self.binop(Operator::I32LtU, rel, two, Type::I32);
        let low = self.unop(Operator::I32WrapI64, v, Type::I32);
        let z = self.i32c(0);
        let nz = self.binop(Operator::I32Ne, low, z, Type::I32);
        let tagged_truth = self.binop(Operator::I32And, has_payload, nz, Type::I32);
        // A double: neither ±0 nor NaN, i.e. |x| > 0.
        let clear = self.i32c(TAG_CLEAR);
        let is_double = self.binop(Operator::I32LtU, t, clear, Type::I32);
        let x = self.unop(Operator::F64ReinterpretI64, v, Type::F64);
        let ax = self.unop(Operator::F64Abs, x, Type::F64);
        let fz = self.f64c(0.0);
        let double_truth = self.binop(Operator::F64Gt, ax, fz, Type::I32);
        let inline = self.binop(Operator::I32Or, tagged, is_double, Type::I32);
        let truth = self.select(Type::I32, double_truth, tagged_truth, is_double);
        (inline, truth)
    }

    /// Branch on ToBoolean(`v`): inline for the `truthy_inline` tags, else
    /// through the `to_boolean` helper.
    pub(super) fn branch_truthy(&mut self, v: Value, if_true: BlockTarget, if_false: BlockTarget) {
        let (inline, truth) = self.truthy_inline(v);
        let fast_b = self.body.add_block();
        let slow_b = self.body.add_block();
        self.cond_br(inline, Self::goto(fast_b), Self::goto(slow_b));
        (self.cur, self.live) = (fast_b, true);
        self.cond_br(truth, if_true.clone(), if_false.clone());
        (self.cur, self.live) = (slow_b, true);
        let t = self.truthy(v);
        self.cond_br(t, if_true, if_false);
    }

    /// `==`/`===` decided inline: both values have the same tag, and it is
    /// int32, boolean, undefined, null or object. With one tag, loose and
    /// strict equality agree, and are identity of the raw bits (payloads
    /// of undefined and null are not compared). Doubles (NaN, -0),
    /// strings and BigInts are left to the helper. Returns (whether
    /// inline, equal if so).
    pub(super) fn equality_inline(&mut self, a: Value, b: Value) -> (Value, Value) {
        let ta = self.tag_of(a);
        let tb = self.tag_of(b);
        let same = self.binop(Operator::I32Eq, ta, tb, Type::I32);
        let base = self.i32c(TAG_INT32 as u32);
        let rel = self.binop(Operator::I32Sub, ta, base, Type::I32);
        let four = self.i32c(4);
        let prim = self.binop(Operator::I32LtU, rel, four, Type::I32);
        let obj_k = self.i32c(TAG_OBJECT as u32);
        let obj = self.binop(Operator::I32Eq, ta, obj_k, Type::I32);
        let kind_ok = self.binop(Operator::I32Or, prim, obj, Type::I32);
        let inline = self.binop(Operator::I32And, same, kind_ok, Type::I32);
        let bits_eq = self.binop(Operator::I64Eq, a, b, Type::I32);
        let two = self.i32c(2);
        let unit = self.binop(Operator::I32GeU, rel, two, Type::I32);
        let unit = self.binop(Operator::I32And, unit, prim, Type::I32);
        let eq = self.binop(Operator::I32Or, bits_eq, unit, Type::I32);
        (inline, eq)
    }

    pub(super) fn push_const(&mut self, k: u32, bits: u64) {
        let v = self.i64c(bits);
        self.set_slot(k, v);
    }

    /// `[a] -> [f(a)]`.
    pub(super) fn unary(&mut self, f: Func, d: u32) {
        let a = self.slot(d - 1);
        let r = self.rt(f, &[a]);
        self.set_slot(d - 1, r);
    }

    /// `[a, b] -> [f(a, b, extra...)]`.
    pub(super) fn binary(&mut self, f: Func, extra: &[Value], d: u32) {
        let a = self.slot(d - 2);
        let b = self.slot(d - 1);
        let mut args = vec![a, b];
        args.extend_from_slice(extra);
        let r = self.rt(f, &args);
        self.set_slot(d - 2, r);
    }

    /// `[a, b] -> [compare(kind, a, b)]` through the helper.
    pub(super) fn compare(&mut self, kind: u32, a: Value, b: Value) {
        let k = self.i32c(kind);
        let r = self.rt(self.h.compare, &[k, a, b]);
        self.set_slot(self.d - 2, r);
    }
}
