//! op objects: the lowering of one run of arms of `Gen::op`, split out of `codegen.rs` unchanged.

use super::*;

impl<'a> Gen<'a> {
    pub(super) fn op_objects(&mut self, p: &mut BytecodeParser, op: JSOp) -> R<bool> {
        use JSOp::*;
        let d = self.d;
        let h = self.h;
        match op {
            // --- literals ---
            NewInit | NewObject => {
                skip(p, op);
                let site = crate::ids::Site::new(self.sid, self.pc);
                let z = self.i32c(0);
                let n = self.i32c(self.ctx.lit_nslots_in.get(&site).copied().unwrap_or(0));
                let r = self.rt(h.new_object, &[z, n]);
                let w = self
                    .ctx
                    .lit_stamps_in
                    .get(&site)
                    .map(|&l| (l + 1) | CLASS_WORD_SLOTS);
                self.stamp_fresh(r, w);
                self.set_slot(d, r);
            }
            NewArray => {
                let len = p.next_uint32().unwrap();
                let (lv, z) = (self.i32c(len), self.i32c(0));
                let r = self.rt(h.new_array, &[lv, z]);
                let site = crate::ids::Site::new(self.sid, self.pc);
                let w = self.ctx.array_stamp_in.get(&site).copied();
                self.stamp_fresh(r, w);
                self.set_slot(d, r);
            }
            InitProp | InitHiddenProp | InitLockedProp => {
                let a = self.atom(p.next_uint32().unwrap())?;
                let attrs = match op {
                    InitProp => INIT_ATTR_ENUMERATE,
                    InitHiddenProp => INIT_ATTR_HIDDEN,
                    _ => INIT_ATTR_LOCKED,
                };
                let obj = self.slot(d - 2);
                let v = self.slot(d - 1);
                let (av, at, none) = (self.i32c(a), self.i32c(attrs), self.i32c(u32::MAX));
                self.rt(h.init_prop, &[obj, av, v, at, none]);
            }
            InitElem | InitHiddenElem | InitLockedElem => {
                let attrs = match op {
                    InitElem => INIT_ATTR_ENUMERATE,
                    InitHiddenElem => INIT_ATTR_HIDDEN,
                    _ => INIT_ATTR_LOCKED,
                };
                let obj = self.slot(d - 3);
                let key = self.slot(d - 2);
                let v = self.slot(d - 1);
                let at = self.i32c(attrs);
                self.rt(h.init_elem, &[obj, key, v, at]);
            }
            InitElemArray => {
                let index = p.next_uint32().unwrap();
                let obj = self.slot(d - 2);
                let v = self.slot(d - 1);
                let key = self.i64c((TAG_INT32 << 32) | u64::from(index));
                let at = self.i32c(INIT_ATTR_ENUMERATE);
                self.rt(h.init_elem, &[obj, key, v, at]);
            }
            InitElemInc => {
                let obj = self.slot(d - 3);
                let idx = self.slot(d - 2);
                let v = self.slot(d - 1);
                let at = self.i32c(INIT_ATTR_ENUMERATE);
                self.rt(h.init_elem, &[obj, idx, v, at]);
                // The index is an int32 (array literal positions are small).
                let idx = self.slot(d - 2);
                let i = self.unop(Operator::I32WrapI64, idx, Type::I32);
                let one = self.i32c(1);
                let n = self.binop(Operator::I32Add, i, one, Type::I32);
                let r = self.boxed_int32(n);
                self.set_slot(d - 2, r);
            }
            MutateProto => {
                let obj = self.slot(d - 2);
                let proto = self.slot(d - 1);
                self.rt(h.mutate_proto, &[obj, proto]);
            }
            InitHomeObject => {
                let f = self.slot(d - 2);
                let home = self.slot(d - 1);
                self.rt(h.init_home_object, &[f, home]);
            }
            InitPropGetter | InitHiddenPropGetter | InitPropSetter | InitHiddenPropSetter => {
                let a = self.atom(p.next_uint32().unwrap())?;
                let kind = u32::from(matches!(op, InitPropSetter | InitHiddenPropSetter))
                    | (u32::from(matches!(op, InitHiddenPropGetter | InitHiddenPropSetter)) << 1);
                let obj = self.slot(d - 2);
                let f = self.slot(d - 1);
                let (av, kv) = (self.i32c(a), self.i32c(kind));
                self.rt(h.init_prop_getset, &[obj, av, f, kv]);
            }
            InitElemGetter | InitHiddenElemGetter | InitElemSetter | InitHiddenElemSetter => {
                let kind = u32::from(matches!(op, InitElemSetter | InitHiddenElemSetter))
                    | (u32::from(matches!(op, InitHiddenElemGetter | InitHiddenElemSetter)) << 1);
                let obj = self.slot(d - 3);
                let key = self.slot(d - 2);
                let f = self.slot(d - 1);
                let kv = self.i32c(kind);
                self.rt(h.init_elem_getset, &[obj, key, f, kv]);
            }
            NewPrivateName => {
                let a = self.atom(p.next_uint32().unwrap())?;
                let av = self.i32c(a);
                let r = self.rt(h.new_private_name, &[av]);
                self.set_slot(d, r);
            }
            CheckPrivateField => {
                let cond = p.next_uint8().unwrap();
                let kind = p.next_uint8().unwrap();
                let obj = self.slot(d - 2);
                let key = self.slot(d - 1);
                let (cv, kv) = (self.i32c(u32::from(cond)), self.i32c(u32::from(kind)));
                let r = self.rt(h.check_private_field, &[obj, key, cv, kv]);
                self.set_slot(d, r);
            }

            // --- exceptions ---
            Exception => {
                let r = self.rt(h.exception, &[]);
                self.set_slot(d, r);
            }
            ExceptionAndStack => {
                let exc = self.top_addr(d);
                let stk = self.top_addr(d + 1);
                let gef = h.get_exception_for_finally;
                let ok = self
                    .call(gef, &[self.cx, exc, stk], Some(Type::I32))
                    .unwrap();
                self.branch_on_err(ok);
            }

            // --- iteration ---
            Iter => self.unary(h.iter_, d),
            MoreIter => {
                let it = self.slot(d - 1);
                let r = self
                    .call(h.more_iter, &[self.cx, it], Some(Type::I64))
                    .unwrap();
                self.set_slot(d, r);
            }
            IsNoIter => {
                let v = self.slot(d - 1);
                let m = self.i64c((TAG_MAGIC << 32) | MAGIC_NO_ITER_VALUE);
                let b = self.binop(Operator::I64Eq, v, m, Type::I32);
                let r = self.boxed_bool(b);
                self.set_slot(d, r);
            }
            EndIter => {
                let it = self.slot(d - 2);
                self.call(h.end_iter, &[self.cx, it], None);
            }
            OptimizeGetIterator => {
                let v = self.slot(d - 1);
                let b = self
                    .call(h.optimize_get_iterator, &[self.cx, v], Some(Type::I32))
                    .unwrap();
                let r = self.boxed_bool(b);
                self.set_slot(d - 1, r);
            }
            CloseIter => {
                let kind = p.next_uint8().unwrap();
                let it = self.slot(d - 1);
                let k = self.i32c(u32::from(kind));
                self.rt(h.close_iter, &[it, k]);
            }
            ToAsyncIter => self.binary(h.to_async_iter, &[], d),
            _ => return Ok(false),
        }
        Ok(true)
    }
}
