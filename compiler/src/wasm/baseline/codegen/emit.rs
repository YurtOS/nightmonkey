//! emit: methods of `Gen`, split out of `codegen.rs` unchanged.

use super::*;

impl<'a> Gen<'a> {
    pub(super) fn push_val(&mut self, def: ValueDef) -> Value {
        let v = self.body.add_value(def);
        self.body.append_to_block(self.cur, v);
        v
    }

    pub(super) fn i32c(&mut self, value: u32) -> Value {
        let ty = self.body.single_type_list(Type::I32);
        self.push_val(ValueDef::Operator(
            Operator::I32Const { value },
            Default::default(),
            ty,
        ))
    }

    pub(super) fn i64c(&mut self, value: u64) -> Value {
        let ty = self.body.single_type_list(Type::I64);
        self.push_val(ValueDef::Operator(
            Operator::I64Const { value },
            Default::default(),
            ty,
        ))
    }

    pub(super) fn unop(&mut self, op: Operator, a: Value, t: Type) -> Value {
        let args = self.body.arg_pool.single(a);
        let ty = self.body.single_type_list(t);
        self.push_val(ValueDef::Operator(op, args, ty))
    }

    pub(super) fn binop(&mut self, op: Operator, a: Value, b: Value, t: Type) -> Value {
        let args = self.body.arg_pool.double(a, b);
        let ty = self.body.single_type_list(t);
        self.push_val(ValueDef::Operator(op, args, ty))
    }

    pub(super) fn select(&mut self, t: Type, a: Value, b: Value, cond: Value) -> Value {
        let args = self.body.arg_pool.from_iter([a, b, cond].into_iter());
        let ty = self.body.single_type_list(t);
        self.push_val(ValueDef::Operator(
            Operator::TypedSelect { ty: t },
            args,
            ty,
        ))
    }

    pub(super) fn mem(&self, align: u32, offset: u32) -> MemoryArg {
        MemoryArg {
            align,
            offset,
            memory: self.h.mem,
        }
    }

    pub(super) fn load_i64(&mut self, addr: Value, offset: u32) -> Value {
        let m = self.mem(3, offset);
        self.unop(Operator::I64Load { memory: m }, addr, Type::I64)
    }

    pub(super) fn load_i32(&mut self, addr: Value, offset: u32) -> Value {
        let m = self.mem(2, offset);
        self.unop(Operator::I32Load { memory: m }, addr, Type::I32)
    }

    pub(super) fn store_i64(&mut self, addr: Value, offset: u32, value: Value) {
        let m = self.mem(3, offset);
        let args = self.body.arg_pool.double(addr, value);
        self.push_val(ValueDef::Operator(
            Operator::I64Store { memory: m },
            args,
            Default::default(),
        ));
    }

    pub(super) fn store_i32(&mut self, addr: Value, offset: u32, value: Value) {
        let m = self.mem(2, offset);
        let args = self.body.arg_pool.double(addr, value);
        self.push_val(ValueDef::Operator(
            Operator::I32Store { memory: m },
            args,
            Default::default(),
        ));
    }

    pub(super) fn add_off(&mut self, addr: Value, off: u32) -> Value {
        if off == 0 {
            return addr;
        }
        let k = self.i32c(off);
        self.binop(Operator::I32Add, addr, k, Type::I32)
    }

    pub(super) fn call(&mut self, f: Func, args: &[Value], ret: Option<Type>) -> Option<Value> {
        let args = self.body.arg_pool.from_iter(args.iter().copied());
        let ty = match ret {
            Some(t) => self.body.single_type_list(t),
            None => Default::default(),
        };
        let v = self.push_val(ValueDef::Operator(
            Operator::Call { function_index: f },
            args,
            ty,
        ));
        ret.map(|_| v)
    }

    pub(super) fn terminate(&mut self, t: Terminator) {
        self.body.set_terminator(self.cur, t);
        self.live = false;
    }

    pub(super) fn br(&mut self, target: BlockTarget) {
        self.terminate(Terminator::Br { target });
    }

    pub(super) fn cond_br(&mut self, cond: Value, if_true: BlockTarget, if_false: BlockTarget) {
        self.terminate(Terminator::CondBr {
            cond,
            if_true,
            if_false,
        });
    }

    pub(super) fn goto(block: Block) -> BlockTarget {
        BlockTarget {
            block,
            args: vec![],
        }
    }

    /// Start emitting into a fresh block.
    pub(super) fn fresh(&mut self) -> Block {
        let b = self.body.add_block();
        self.cur = b;
        self.live = true;
        b
    }

    /// Operand slot `k`.
    pub(super) fn slot(&mut self, k: u32) -> Value {
        self.load_i64(self.vp, self.layout.operand(k))
    }

    pub(super) fn set_slot(&mut self, k: u32, v: Value) {
        self.store_i64(self.vp, self.layout.operand(k), v)
    }

    pub(super) fn top_addr(&mut self, depth: u32) -> Value {
        self.add_off(self.vp, self.layout.top(depth))
    }

    pub(super) fn env(&mut self) -> Value {
        self.load_i64(self.vp, self.layout.env())
    }

    pub(super) fn set_env(&mut self, v: Value) {
        self.store_i64(self.vp, self.layout.env(), v)
    }

    pub(super) fn boxed_bool(&mut self, b: Value) -> Value {
        let b64 = self.unop(Operator::I64ExtendI32U, b, Type::I64);
        let tag = self.i64c(TAG_BOOLEAN << 32);
        self.binop(Operator::I64Or, tag, b64, Type::I64)
    }

    pub(super) fn boxed_int32(&mut self, i: Value) -> Value {
        let i64v = self.unop(Operator::I64ExtendI32U, i, Type::I64);
        let tag = self.i64c(TAG_INT32 << 32);
        self.binop(Operator::I64Or, tag, i64v, Type::I64)
    }

    /// The value's tag (high word).
    pub(super) fn tag_of(&mut self, v: Value) -> Value {
        let sh = self.i64c(32);
        let hi = self.binop(Operator::I64ShrU, v, sh, Type::I64);
        self.unop(Operator::I32WrapI64, hi, Type::I32)
    }

    /// Whether a value of tag `tag` is of claimed type `c` (a typed-array
    /// claim's kind is not checked here: no).
    pub(super) fn tag_in_claim(&mut self, tag: Value, c: crate::facts::Claim) -> Value {
        use crate::opsem::*;
        if c.ta_kind().is_some() {
            return self.i32c(0);
        }
        let p = c.prims();
        let singles = [
            (PRIM_INT32, TAG_INT32 as u32),
            (PRIM_BOOLEAN, TAG_BOOLEAN as u32),
            (PRIM_UNDEFINED, TAG_UNDEFINED as u32),
            (PRIM_NULL, TAG_NULL as u32),
            (PRIM_STRING, TAG_STRING as u32),
            (PRIM_SYMBOL, TAG_SYMBOL as u32),
            (PRIM_BIGINT, TAG_BIGINT_HI),
        ];
        let mut acc = self.i32c(0);
        for (prim, t) in singles {
            if p.intersects(prim) {
                let k = self.i32c(t);
                let e = self.binop(Operator::I32Eq, tag, k, Type::I32);
                acc = self.binop(Operator::I32Or, acc, e, Type::I32);
            }
        }
        if p.intersects(PRIM_DOUBLE) {
            let k = self.i32c(TAG_CLEAR);
            let e = self.binop(Operator::I32LtU, tag, k, Type::I32);
            acc = self.binop(Operator::I32Or, acc, e, Type::I32);
        }
        if c.bits() & crate::facts::Claim::OBJECT.bits() != 0 {
            let k = self.i32c(TAG_OBJECT as u32);
            let e = self.binop(Operator::I32Eq, tag, k, Type::I32);
            acc = self.binop(Operator::I32Or, acc, e, Type::I32);
        }
        acc
    }

    pub(super) fn tag_eq(&mut self, v: Value, tag: u64) -> Value {
        let t = self.tag_of(v);
        let k = self.i32c(tag as u32);
        self.binop(Operator::I32Eq, t, k, Type::I32)
    }

    /// This frame's `JSScript*`, re-derived from the rooted callee slot
    /// (the ABI's script parameter does not survive a compacting GC).
    pub(super) fn script_ptr(&mut self) -> Value {
        let slot0 = self.load_i64(self.sp, 0);
        let low = self.unop(Operator::I32WrapI64, slot0, Type::I32);
        if self.is_global {
            return low;
        }
        self.load_i32(low, FUNC_SCRIPT_SLOT_OFFSET)
    }

    pub(super) fn atom(&mut self, name_index: u32) -> R<u32> {
        let gc = *self
            .script
            .gcthings
            .get(usize::try_from(name_index).unwrap())
            .ok_or_else(|| format!("name index {name_index} out of range"))?;
        match self.ctx.source.object(gc) {
            SourceObject::String(s) => {
                let name = self.atoms.names.intern(s);
                Ok(self.atoms.intern(name))
            }
            _ => Err(format!("name index {name_index} is not a string")),
        }
    }

    /// A may-GC, may-throw helper: `helper(cx, top, args...)` with `top`
    /// above the operand stack at the op's entry depth. Branches to the
    /// op's exception landing on failure; returns the boxed result the
    /// helper wrote to `*top` (meaningless for helpers without one).
    pub(super) fn rt(&mut self, f: Func, args: &[Value]) -> Value {
        let top = self.top_addr(self.d);
        let mut full = vec![self.cx, top];
        full.extend_from_slice(args);
        let ok = self.call(f, &full, Some(Type::I32)).unwrap();
        self.branch_on_err(ok);
        let top = self.top_addr(self.d);
        self.load_i64(top, 0)
    }

    /// A may-GC helper that always throws.
    pub(super) fn rt_throw(&mut self, f: Func, args: &[Value]) {
        let top = self.top_addr(self.d);
        let mut full = vec![self.cx, top];
        full.extend_from_slice(args);
        self.call(f, &full, None);
        let t = self.exc_target(self.pc, self.d);
        self.br(t);
    }

    pub(super) fn f64c(&mut self, x: f64) -> Value {
        let ty = self.body.single_type_list(Type::F64);
        self.push_val(ValueDef::Operator(
            Operator::F64Const { value: x.to_bits() },
            Default::default(),
            ty,
        ))
    }
}
