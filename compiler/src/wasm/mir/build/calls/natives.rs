//! natives: methods of `Run`, split out of `calls.rs` unchanged.

use super::*;

impl<'s, 'a> Run<'s, 'a> {
    /// Push `x`, an operand this op popped, back: as its fence renamed it,
    /// if one did (`fence_params`).
    /// `recv.length` as a typed op (§7 `length.{string,array}`), pushing
    /// the int32 it is: a proven string's length word; for a receiver the
    /// builder has evidence is an array (a native object its element
    /// accesses guarded, or an array layout's value class), its elements
    /// header's, guarded to an array and to int32 (a miss exits). False,
    /// with nothing emitted, for anything else (the generic read, whose
    /// lowering tests the same cases at run time).
    pub(in crate::wasm::mir::build) fn length_op(&mut self, recv: Slot) -> bool {
        if let Ty::Val(t) = recv.ty {
            if t.is_nonempty_subset_of(TagSet::STRING) {
                let s = self.inst(
                    Opcode::Unbox(UnboxKind::Str),
                    vec![recv.v],
                    Some(MType::STR_TOP),
                );
                let n = self.inst(
                    Opcode::LengthString,
                    vec![s],
                    Some(MType::i32_range(0, (1 << 30) - 2)),
                );
                self.push(n, Ty::I32);
                return true;
            }
        }
        let array_cls = |k: u32| self.mm.array_key_min.is_some_and(|min| k + 1 >= min);
        let evidence =
            recv.ty == Ty::Native || self.val_cls.get(&recv.v).is_some_and(|&k| array_cls(k));
        if !evidence || !TagSet::OBJECT.subset_of(recv.ty.tags()) {
            return false;
        }
        let o = match recv.ty {
            Ty::Native | Ty::Obj(..) | Ty::ObjHint(_) | Ty::Ctor(..) => recv.v,
            Ty::Val(_) => self.guard(
                Opcode::GuardUnbox(UnboxKind::Obj),
                vec![recv.v],
                MType::OBJ_TOP,
            ),
            _ => return false,
        };
        let arr = MType::Obj(ObjInfo::kind(ObjKind::Array));
        let o = self.guard(Opcode::GuardKind(ObjKind::Array), vec![o], arr);
        let len = self.inst(
            Opcode::LengthArray,
            vec![o],
            Some(MType::int_range(0, u32::MAX.into())),
        );
        let n = self.guard(
            Opcode::IntToI32,
            vec![len],
            MType::i32_range(0, i64::from(i32::MAX)),
        );
        self.push(n, Ty::I32);
        true
    }

    /// `Math.<fn>(args)` of a function with a typed op (`math_fns`), with
    /// `argc` its arity: the callee checked to be the pristine native, the
    /// arguments to be numbers (exiting at the call on a miss: the site
    /// runs in baseline then), and the op on raw f64s, whose result is a
    /// raw f64 rather than a boxed value to guard. False if not such a
    /// call (nothing emitted).
    pub(in crate::wasm::mir::build) fn math_call(&mut self, argc: usize) -> bool {
        let n = self.st.len();
        let Some(&m) = self.math_fns.get(&self.st[n - argc - 2].v) else {
            return false;
        };
        if argc != m.arity() {
            return false;
        }
        let operands: Vec<Slot> = self.st.drain(n - argc - 2..).collect();
        let callee = self.boxed(operands[0]);
        let name = format!("Math.{}", math_fn_name(m));
        let found = self
            .mm
            .natives
            .iter()
            .find(|(_, d)| d.name == name)
            .map(|(id, _)| id);
        let nid = match found {
            Some(id) => id,
            None => self.mm.natives.push(mir::module::NativeDef { name }),
        };
        self.guard(
            Opcode::CheckNative(nid),
            vec![callee],
            MType::Fact(mir::types::FactKind::NativeIntact(nid)),
        );
        let mut args = vec![];
        for x in &operands[2..] {
            let f = if x.ty.num().is_some() {
                self.as_f64(*x)
            } else {
                let b = self.boxed(*x);
                self.guard(
                    Opcode::GuardUnbox(UnboxKind::F64Num),
                    vec![b],
                    MType::F64_TOP,
                )
            };
            args.push(f);
        }
        let r = self.inst(Opcode::Math(m), args, Some(MType::F64_TOP));
        self.push(r, Ty::F64);
        true
    }

    /// `hasOwnProperty.call(o, k)` where the analysis resolved the site's
    /// target to the builtin (`apply_natives`).
    pub(in crate::wasm::mir::build) fn hasown_site(&self) -> bool {
        let site = self.site(self.pc);
        let facts = &self.s.ctx.facts;
        facts.apply_sites.get(&site) == Some(&crate::facts::CallForm::Call)
            && facts.apply_natives.get(&site) == Some(&crate::facts::ApplyNative::HasOwnProperty)
    }

    /// The native forward, operands `call, target, o, k`: with the `.call`
    /// and the target the pristine builtins, the `HasOwn` op on `k, o`;
    /// otherwise the generic call.
    pub(in crate::wasm::mir::build) fn hasown_call(&mut self, vals: &[mir::Value]) -> mir::Value {
        use crate::wasm::translate::{BC_FUN_CALL, BC_OBJ_HASOWN};
        self.demote_for_generic_arm(1);
        let join = self.new_block();
        let result = self.f.add_param(join, MType::VAL_TOP);
        let (check, fast, slow) = (self.new_block(), self.new_block(), self.new_block());
        let is_call = self.inst(
            Opcode::JsIsBuiltin(BC_FUN_CALL),
            vec![vals[0]],
            Some(MType::Bool),
        );
        self.term(
            Opcode::Br,
            vec![is_call],
            vec![Self::goto(check), Self::goto(slow)],
        );
        self.at(check);
        let is_hasown = self.inst(
            Opcode::JsIsBuiltin(BC_OBJ_HASOWN),
            vec![vals[1]],
            Some(MType::Bool),
        );
        self.term(
            Opcode::Br,
            vec![is_hasown],
            vec![Self::goto(fast), Self::goto(slow)],
        );
        for (b, r) in [(fast, true), (slow, false)] {
            self.at(b);
            let r = if r {
                self.js(
                    Opcode::JsRt(RtOp::HasOwn),
                    vec![vals[3], vals[2]],
                    MType::val(TagSet::BOOLEAN),
                )
            } else {
                self.js(Opcode::Call, vals.to_vec(), MType::VAL_TOP)
            };
            self.term(
                Opcode::Jump,
                vec![],
                vec![Edge {
                    block: join,
                    args: vec![EdgeArg::Value(r)],
                }],
            );
        }
        self.at(join);
        result
    }
}
