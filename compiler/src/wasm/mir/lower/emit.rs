//! emit: methods of `Lower`, split out of `lower.rs` unchanged.

use super::*;

impl<'a> Lower<'a> {
    pub(super) fn push_val(&mut self, def: ValueDef) -> Value {
        let v = self.body.add_value(def);
        self.body.append_to_block(self.cur, v);
        v
    }

    pub(super) fn op(&mut self, op: Operator, args: &[Value], ty: Option<Type>) -> Value {
        let args = self.body.arg_pool.from_iter(args.iter().copied());
        let tys = match ty {
            Some(t) => self.body.single_type_list(t),
            None => Default::default(),
        };
        self.push_val(ValueDef::Operator(op, args, tys))
    }

    pub(super) fn i32c(&mut self, value: u32) -> Value {
        self.op(Operator::I32Const { value }, &[], Some(Type::I32))
    }

    pub(super) fn i64c(&mut self, value: u64) -> Value {
        self.op(Operator::I64Const { value }, &[], Some(Type::I64))
    }

    pub(super) fn f64c(&mut self, value: u64) -> Value {
        self.op(Operator::F64Const { value }, &[], Some(Type::F64))
    }

    pub(super) fn un(&mut self, op: Operator, a: Value, t: Type) -> Value {
        self.op(op, &[a], Some(t))
    }

    pub(super) fn bin(&mut self, op: Operator, a: Value, b: Value, t: Type) -> Value {
        self.op(op, &[a, b], Some(t))
    }

    pub(super) fn select(&mut self, t: Type, a: Value, b: Value, cond: Value) -> Value {
        self.op(Operator::TypedSelect { ty: t }, &[a, b, cond], Some(t))
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
        self.un(Operator::I64Load { memory: m }, addr, Type::I64)
    }

    pub(super) fn store_i64(&mut self, addr: Value, offset: u32, v: Value) {
        let m = self.mem(3, offset);
        self.op(Operator::I64Store { memory: m }, &[addr, v], None);
    }

    pub(super) fn store_i32(&mut self, addr: Value, offset: u32, v: Value) {
        let m = self.mem(2, offset);
        self.op(Operator::I32Store { memory: m }, &[addr, v], None);
    }

    pub(super) fn add_off(&mut self, addr: Value, off: u32) -> Value {
        if off == 0 {
            return addr;
        }
        let k = self.i32c(off);
        self.bin(Operator::I32Add, addr, k, Type::I32)
    }

    pub(super) fn call(&mut self, f: Func, args: &[Value], rets: &[Type]) -> Value {
        let args = self.body.arg_pool.from_iter(args.iter().copied());
        let tys = self.body.type_pool.from_iter(rets.iter().copied());
        self.push_val(ValueDef::Operator(
            Operator::Call { function_index: f },
            args,
            tys,
        ))
    }

    pub(super) fn call1(&mut self, f: Func, args: &[Value], ret: Type) -> Value {
        self.call(f, args, &[ret])
    }

    pub(super) fn terminate(&mut self, t: Terminator) {
        self.body.set_terminator(self.cur, t);
    }

    pub(super) fn cond_br(&mut self, cond: Value, if_true: BlockTarget, if_false: BlockTarget) {
        self.terminate(Terminator::CondBr {
            cond,
            if_true,
            if_false,
        });
    }

    pub(super) fn ret(&mut self, err: Value) {
        let flags = self.i32c(FLAGS_ALL);
        self.terminate(Terminator::Return {
            values: vec![err, flags],
        });
    }

    pub(super) fn load_i32(&mut self, addr: Value, offset: u32) -> Value {
        let m = self.mem(2, offset);
        self.un(Operator::I32Load { memory: m }, addr, Type::I32)
    }
}
