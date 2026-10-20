//! op objects: the lowering of one run of arms of `Run::op`, split out of `build.rs` unchanged.

use super::*;

impl<'s, 'a> Run<'s, 'a> {
    pub(super) fn op_objects(&mut self, op: JSOp, pc: Pc, p: &mut BytecodeParser<'a>) -> R<bool> {
        use JSOp::*;
        match op {
            NewInit | NewObject => {
                let n = self
                    .s
                    .ctx
                    .lit_nslots_in
                    .get(&self.site(self.pc))
                    .copied()
                    .unwrap_or(0);
                let lid = self.s.ctx.lit_stamps_in.get(&self.site(self.pc)).copied();
                // A stamped site's allocation is typed (`lit.new`, no
                // fence: MIR-MEMORY.md §4); another's, the generic op.
                let v = match lid {
                    Some(_) => {
                        self.js_static(Opcode::LitNew(n), vec![], MType::val(TagSet::OBJECT))
                    }
                    _ => self.js(
                        Opcode::JsRt(RtOp::NewObject(n)),
                        vec![],
                        MType::val(TagSet::OBJECT),
                    ),
                };
                // An object-literal stamp site: the layout idx with SLOTS
                // (the inits land at the row's slots by construction).
                if let Some(lid) = lid {
                    let w = (lid + 1) | crate::wasm::mir::abi::CLASS_WORD_SLOTS;
                    self.inst(Opcode::StampFresh(w), vec![v], None);
                    self.val_cls.insert(v, lid);
                    self.lit_rows.insert(v, lid);
                }
                self.push(v, Ty::Val(TagSet::OBJECT));
            }
            NewArray => {
                let len = p.next_uint32().unwrap();
                let v = self.js(
                    Opcode::JsRt(RtOp::NewArray(len)),
                    vec![],
                    MType::val(TagSet::OBJECT),
                );
                // An array stamp site: the key with TYPES and RANGES, which
                // an empty array's elements satisfy vacuously; element
                // stores keep them.
                if let Some(&w) = self.s.ctx.array_stamp_in.get(&self.site(self.pc)) {
                    self.inst(Opcode::StampFresh(w), vec![v], None);
                }
                self.push(v, Ty::Val(TagSet::OBJECT));
            }
            InitProp | InitHiddenProp | InitLockedProp => {
                // [obj, v] -> [obj]
                use crate::wasm::mir::abi::{
                    INIT_ATTR_ENUMERATE, INIT_ATTR_HIDDEN, INIT_ATTR_LOCKED,
                };
                let a = self.atom(p.next_uint32().unwrap())?;
                let attrs = match op {
                    InitProp => INIT_ATTR_ENUMERATE,
                    InitHiddenProp => INIT_ATTR_HIDDEN,
                    _ => INIT_ATTR_LOCKED,
                };
                let v = self.pop();
                let o = self.top();
                let (x, y) = (self.boxed(o), self.boxed(v));
                match self.lit_rows.get(&x) {
                    Some(&lid) if attrs == INIT_ATTR_ENUMERATE => {
                        let ok = self.new_block();
                        let err = self.exit_block(true);
                        let key = crate::ids::LayoutKey::new(lid);
                        self.term(
                            Opcode::LitInit(a, key),
                            vec![x, y],
                            vec![Self::goto(ok), Self::goto(err)],
                        );
                        self.at(ok);
                    }
                    _ => self.js_void(Opcode::JsRt(RtOp::InitProp(a, attrs)), vec![x, y]),
                }
            }
            InitElem | InitHiddenElem | InitLockedElem => {
                // [obj, key, v] -> [obj]
                use crate::wasm::mir::abi::{
                    INIT_ATTR_ENUMERATE, INIT_ATTR_HIDDEN, INIT_ATTR_LOCKED,
                };
                let attrs = match op {
                    InitElem => INIT_ATTR_ENUMERATE,
                    InitHiddenElem => INIT_ATTR_HIDDEN,
                    _ => INIT_ATTR_LOCKED,
                };
                let v = self.pop();
                let k = self.pop();
                let o = self.top();
                let r = self.ranges_duty(pc, v);
                let (x, y, z) = (self.boxed(o), self.boxed(k), self.boxed(v));
                self.js_void(Opcode::JsRt(RtOp::InitElem(attrs, r)), vec![x, y, z]);
            }
            InitElemArray => {
                // [obj, v] -> [obj]
                let index = p.next_uint32().unwrap();
                let v = self.pop();
                let o = self.top();
                let k = self.const_val(ConstVal::Int32(index as i32));
                let r = self.ranges_duty(pc, v);
                let (x, z) = (self.boxed(o), self.boxed(v));
                let e = crate::wasm::mir::abi::INIT_ATTR_ENUMERATE;
                self.js_void(Opcode::JsRt(RtOp::InitElem(e, r)), vec![x, k, z]);
            }
            InitElemInc => {
                // [obj, idx, v] -> [obj, idx + 1]: an array literal's
                // spread, its index an int32 (the literal's positions).
                let v = self.pop();
                let i = self.pop();
                if i.ty.num() != Some(Num::I32) {
                    return Err("InitElemInc index not int32".into());
                }
                let o = self.top();
                let r = self.ranges_duty(pc, v);
                let (x, k, z) = (self.boxed(o), self.boxed(i), self.boxed(v));
                let e = crate::wasm::mir::abi::INIT_ATTR_ENUMERATE;
                self.js_void(Opcode::JsRt(RtOp::InitElem(e, r)), vec![x, k, z]);
                let iv = self.as_i32(i);
                let one = self.const_i32(1);
                let n = self.inst(
                    Opcode::I32Wrap(ArithOp::Add),
                    vec![iv, one],
                    Some(MType::I32_TOP),
                );
                self.push(n, Ty::I32);
            }
            ToPropertyKey => {
                // An int32, a string or a symbol is its own key.
                let x = self.top();
                let keyish = TagSet::INT32
                    .union(TagSet::STRING)
                    .union(TagSet::prims(crate::opsem::PRIM_SYMBOL));
                if !x.ty.tags().is_nonempty_subset_of(keyish) {
                    let x = self.pop();
                    let v = self.boxed(x);
                    let r = self.js(Opcode::JsRt(RtOp::ToPropertyKey), vec![v], MType::VAL_TOP);
                    self.push(r, Ty::Val(TagSet::ALL));
                }
            }
            Typeof | TypeofExpr => {
                let a = self.pop();
                let x = self.boxed(a);
                let v = self.inst(Opcode::JsTypeof, vec![x], Some(MType::val(TagSet::STRING)));
                self.push(v, Ty::Val(TagSet::STRING));
            }
            Throw => {
                let a = self.pop();
                let x = self.boxed(a);
                let err = self.exit_block(true);
                self.retain_locals(&Opcode::JsThrow);
                let (t, _) = self.f.add_inst(
                    self.cur,
                    Opcode::JsThrow,
                    vec![x],
                    &[],
                    vec![Self::goto(err)],
                );
                self.live = false;
                self.f.witnesses[t] = Some(mir::func::Witness {
                    may_kill: mir::types::KillPattern::ALL,
                });
            }
            // The actuals (the frame's variable region is past them: the
            // lowering's `vp`).
            Arguments => {
                if self.s.apply_fwd.is_some() || self.s.args_reads.is_some() {
                    // Only forwarded (`apply_forward`): never made, unless an
                    // exit needs it (`exit_operands`). A value of its own,
                    // so no other undefined is mistaken for it.
                    let v = self.const_val(ConstVal::Undefined);
                    self.args_placeholder = Some(v);
                    self.push(v, Ty::Val(TagSet::prims(PRIM_UNDEFINED)));
                } else {
                    let v = self.js_static(Opcode::ArgsObject, vec![], MType::val(TagSet::OBJECT));
                    self.push(v, Ty::Val(TagSet::OBJECT));
                }
            }
            Rest => {
                let nformal = self.s.nargs.saturating_sub(1);
                let v = self.js_static(
                    Opcode::RestArray(nformal),
                    vec![],
                    MType::val(TagSet::OBJECT),
                );
                self.push(v, Ty::Val(TagSet::OBJECT));
            }
            ArgumentsLength => {
                let v = self.inst(Opcode::ArgsLength, vec![], Some(MType::I32_TOP));
                self.push(v, Ty::I32);
            }
            GetActualArg => {
                let i = self.pop();
                if i.ty != Ty::I32 {
                    return Err("GetActualArg of a non-int32 index".into());
                }
                let v = self.inst(Opcode::ActualArg, vec![i.v], Some(MType::VAL_TOP));
                self.push(v, Ty::Val(TagSet::ALL));
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
}
