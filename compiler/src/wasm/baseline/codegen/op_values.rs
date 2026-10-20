//! op values: the lowering of one run of arms of `Gen::op`, split out of `codegen.rs` unchanged.

use super::*;

impl<'a> Gen<'a> {
    pub(super) fn op_values(&mut self, p: &mut BytecodeParser, op: JSOp) -> R<bool> {
        use JSOp::*;
        let d = self.d;
        let h = self.h;
        match op {
            GetIntrinsic => {
                let a = self.atom(p.next_uint32().unwrap())?;
                let av = self.i32c(a);
                let r = self.rt(h.get_intrinsic, &[av]);
                self.set_slot(d, r);
            }
            Object | CallSiteObj => {
                let idx = p.next_uint32().unwrap();
                let iv = self.i32c(idx);
                let script = self.script_ptr();
                let r = self
                    .call(h.object, &[self.cx, script, iv], Some(Type::I64))
                    .unwrap();
                self.set_slot(d, r);
            }
            Symbol => {
                let code = p.next_uint8().unwrap();
                let c = self.i32c(u32::from(code));
                let r = self.call(h.symbol, &[self.cx, c], Some(Type::I64)).unwrap();
                self.set_slot(d, r);
            }
            RegExp => {
                let idx = p.next_uint32().unwrap();
                let iv = self.i32c(idx);
                let script = self.script_ptr();
                let r = self.rt(h.regexp, &[script, iv]);
                self.set_slot(d, r);
            }
            BuiltinObject => {
                let kind = p.next_uint8().unwrap();
                let k = self.i32c(u32::from(kind));
                let r = self.rt(h.builtin_object, &[k]);
                self.set_slot(d, r);
            }

            // --- checks and coercions ---
            IsNullOrUndefined => {
                let v = self.slot(d - 1);
                let u = self.tag_eq(v, TAG_UNDEFINED);
                let n = self.tag_eq(v, TAG_NULL);
                let b = self.binop(Operator::I32Or, u, n, Type::I32);
                let r = self.boxed_bool(b);
                self.set_slot(d, r);
            }
            ToString => self.unary(h.tostring, d),
            ToPropertyKey => self.unary(h.to_property_key, d),
            ToNumeric => {
                // Identity on a number (int32 or double).
                let v = self.slot(d - 1);
                let is_num = self.is_number(v);
                self.fast_path(is_num, |_, _| {}, |g| g.unary(h.tonumeric, d));
            }
            Pos => {
                // Identity on a number.
                let v = self.slot(d - 1);
                let is_num = self.is_number(v);
                self.fast_path(is_num, |_, _| {}, |g| g.unary(h.pos, d));
            }
            ImplicitThis => self.unary(h.implicit_this, d),
            ObjWithProto => self.unary(h.obj_with_proto, d),
            OptimizeSpreadCall => self.unary(h.optimize_spread_call, d),
            CheckObjCoercible => self.check(h.check_obj_coercible, &[]),
            CheckClassHeritage => self.check(h.check_class_heritage, &[]),
            CheckThis => self.check(h.check_this, &[]),
            CheckThisReinit => self.check(h.check_this_reinit, &[]),
            CheckIsObj => {
                let kind = p.next_uint8().unwrap();
                let k = self.i32c(u32::from(kind));
                self.check(h.check_is_obj, &[k]);
            }
            CheckLexical | CheckAliasedLexical => {
                skip(p, op);
                let script = self.script_ptr();
                let pcv = self.i32c(self.pc.get());
                self.check(h.check_lexical, &[script, pcv]);
            }
            CheckReturn => {
                let thisv = self.slot(d - 1);
                let rval = self.load_i64(self.vp, self.layout.rval());
                let r = self.rt(h.check_return, &[thisv, rval]);
                self.set_slot(d - 1, r);
            }
            ThrowSetConst => {
                skip(p, op);
                let script = self.script_ptr();
                let pcv = self.i32c(self.pc.get());
                self.rt_throw(h.throw_set_const, &[script, pcv]);
            }
            ThrowMsg => {
                let kind = p.next_uint8().unwrap();
                let k = self.i32c(u32::from(kind));
                self.rt_throw(h.throw_msg, &[k]);
            }
            Throw => {
                let v = self.slot(d - 1);
                self.rt_throw(h.throw, &[v]);
            }
            ThrowWithStack => {
                let v = self.slot(d - 2);
                let s = self.slot(d - 1);
                self.rt_throw(h.throw_with_stack, &[v, s]);
            }

            // --- frame values ---
            Callee => {
                let c = self.load_i64(self.sp, 0);
                self.set_slot(d, c);
            }
            NewTarget => {
                let nt = self.load_i64(self.vp, self.layout.new_target());
                self.set_slot(d, nt);
            }
            FunctionThis => self.function_this(),
            GlobalThis => {
                let r = self.rt(h.global_this, &[]);
                self.set_slot(d, r);
            }
            ArgumentsLength => {
                let r = self.boxed_int32(self.argc);
                self.set_slot(d, r);
            }
            GetActualArg => {
                let i = self.slot(d - 1);
                let i = self.unop(Operator::I32WrapI64, i, Type::I32);
                let eight = self.i32c(8);
                let off = self.binop(Operator::I32Mul, i, eight, Type::I32);
                let a = self.binop(Operator::I32Add, self.sp, off, Type::I32);
                let v = self.load_i64(a, FrameLayout::ARGS);
                self.set_slot(d - 1, v);
            }
            Rest => {
                let nformal = u32::from(self.script.nargs).saturating_sub(1);
                let nf = self.i32c(nformal);
                let r = self.rt(h.rest, &[self.sp, self.argc, nf]);
                self.set_slot(d, r);
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
}
