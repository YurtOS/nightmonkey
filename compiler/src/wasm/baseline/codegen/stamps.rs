//! stamps: methods of `Gen`, split out of `codegen.rs` unchanged.

use super::*;

impl<'a> Gen<'a> {
    /// `night_runtime_set_property`'s vouch bit (flag 2) for a store of
    /// `val` to field `atom` through `recv`, by the MIR tier's rule
    /// (`field_classes`): 2 when `recv` is an object whose class (its
    /// stamp's identity, or its early key while constructed) types the
    /// field and `val` is of that type, or a class typing no such field;
    /// else 0. The engine then keeps its TYPES across a store that runs no
    /// JS, so a baseline episode (after an exit, say) does not demote the
    /// conforming objects it writes, which would fail OPT's guards for good.
    pub(super) fn vouch_types(&mut self, atom: u32, recv: Value, val: Value) -> Value {
        let name = self.atoms.name_of(atom);
        let classes = crate::wasm::mir::stamp::field_classes(self.ctx, name);
        if classes.is_empty() || classes.len() > crate::wasm::mir::stamp::MAX_STORE_CLASSES {
            return self.i32c(0);
        }
        let is_obj = self.tag_eq(recv, TAG_OBJECT);
        let o = self.unop(Operator::I32WrapI64, recv, Type::I32);
        let z = self.i32c(0);
        // A non-object's word is read off the null page, then ignored.
        let obj = self.select(Type::I32, o, z, is_obj);
        let w = self.load_i32(obj, OBJ_CLASS_IDX_OFFSET);
        let m16 = self.i32c(0xFFFF);
        let idx = self.binop(Operator::I32And, w, m16, Type::I32);
        let ksh = self.i32c(EARLY_KEY_SHIFT);
        let kraw = self.binop(Operator::I32ShrU, w, ksh, Type::I32);
        let km = self.i32c(EARLY_KEY_MAX);
        let early = self.binop(Operator::I32And, kraw, km, Type::I32);
        let sb = self.i32c(CLASS_WORD_SENTINEL);
        let sent = self.binop(Operator::I32And, w, sb, Type::I32);
        let id = self.select(Type::I32, early, idx, sent);
        let tag = self.tag_of(val);
        let mut ok = self.i32c(0);
        let mut listed = self.i32c(0);
        for (k, c) in classes {
            let kv = self.i32c(k);
            let is = self.binop(Operator::I32Eq, id, kv, Type::I32);
            listed = self.binop(Operator::I32Or, listed, is, Type::I32);
            let conf = self.tag_in_claim(tag, c);
            let hit = self.binop(Operator::I32And, is, conf, Type::I32);
            ok = self.binop(Operator::I32Or, ok, hit, Type::I32);
        }
        let unlisted = self.unop(Operator::I32Eqz, listed, Type::I32);
        let keyed = self.binop(Operator::I32Ne, id, z, Type::I32);
        let other = self.binop(Operator::I32And, unlisted, keyed, Type::I32);
        let ok = self.binop(Operator::I32Or, ok, other, Type::I32);
        let ok = self.binop(Operator::I32And, ok, is_obj, Type::I32);
        let one = self.i32c(1);
        self.binop(Operator::I32Shl, ok, one, Type::I32)
    }

    pub(super) fn restamp(&mut self, v: Value, r: [u32; 7]) {
        let mut args = vec![v];
        for x in r {
            args.push(self.i32c(x));
        }
        self.call(self.h.ctor_restamp, &args, None);
    }

    /// Formal `n`'s current value.
    pub(super) fn formal(&mut self, n: u32) -> Value {
        if self.script.has_mapped_args {
            let obj = self.load_i64(self.vp, self.layout.args_obj());
            let i = self.i32c(n);
            self.call(self.h.get_mapped_arg, &[obj, i], Some(Type::I64))
                .unwrap()
        } else {
            self.load_i64(self.sp, self.layout.arg(n))
        }
    }

    /// After the last add of a post-construction fill sequence
    /// (`local_restamps_in`), the restamp of the named local or formal.
    pub(super) fn local_restamp(&mut self, pc: Pc) {
        let site = crate::ids::Site::new(self.sid, pc);
        let Some(&(local, ref si)) = self.ctx.local_restamps_in.get(&site) else {
            return;
        };
        let Some(r) = typed_restamp_args(self.ctx, si) else {
            return;
        };
        let v = if local & crate::facts::RESTAMP_FORMAL != 0 {
            self.formal(local & !crate::facts::RESTAMP_FORMAL)
        } else {
            self.load_i64(self.vp, self.layout.local(local))
        };
        self.restamp(v, r);
    }

    /// Write stamp word `w` into fresh object `r` (the literal and array
    /// allocation stamps).
    pub(super) fn stamp_fresh(&mut self, r: Value, w: Option<u32>) {
        if let Some(w) = w {
            let obj = self.unop(Operator::I32WrapI64, r, Type::I32);
            let wv = self.i32c(w);
            self.store_i32(obj, OBJ_CLASS_IDX_OFFSET, wv);
        }
    }
}
