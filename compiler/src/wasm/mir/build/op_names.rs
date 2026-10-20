//! op names: the lowering of one run of arms of `Run::op`, split out of `build.rs` unchanged.

use super::*;

impl<'s, 'a> Run<'s, 'a> {
    pub(super) fn op_names(&mut self, op: JSOp, pc: Pc, p: &mut BytecodeParser<'a>) -> R<bool> {
        use JSOp::*;
        match op {
            // --- generic names, properties, elements and calls ---
            GetGName => {
                let index = p.next_uint32().unwrap();
                if self.s.next_is_typeof(pc, op) {
                    // `typeof name`: an unbound global reads as undefined.
                    let a = self.atom(index)?;
                    let r = self.js(Opcode::JsRt(RtOp::GetNameTypeof(a)), vec![], MType::VAL_TOP);
                    self.push(r, Ty::Val(TagSet::ALL));
                    return Ok(true);
                }
                let a = self.atom(index)?;
                if let Some(&fg) = self.s.fused.get(&index) {
                    if self.fused_gname(a, fg) {
                        return Ok(true);
                    }
                }
                let r = match self.s.syn_bids.get(&index) {
                    Some(&slot) => {
                        let b = self.binding(a, slot);
                        if let Some(k) = self.mm.bindings[b].pred {
                            // A predicted function: while the binding's
                            // value fuse says it holds a compiled function
                            // of script `k`, the read is that function,
                            // typed (a call through it needs no callee
                            // guard); else exit here.
                            let fact = self.guard(
                                Opcode::CheckBinding(b, BindingCheck::Fn),
                                vec![],
                                MType::Fact(mir::types::FactKind::BindingFn(b)),
                            );
                            let t = MType::Val(mir::types::VSet::new(
                                TagSet::OBJECT,
                                mir::types::NumInfo::TOP,
                                ObjInfo::kind(ObjKind::Function(Some(k))),
                                mir::types::StrInfo::TOP,
                            ));
                            let r = self.inst(Opcode::LoadGName(b), vec![fact], Some(t));
                            self.gname_vals.insert(r, a);
                            self.push(r, Ty::Fn(k));
                            return Ok(true);
                        }
                        // §3: `check.binding`, then a leaf load of the
                        // slot. A syntactic global is a non-configurable
                        // data property, so the check holds once resolved;
                        // a miss exits here.
                        let fact = self.guard(
                            Opcode::CheckBinding(b, BindingCheck::Read),
                            vec![],
                            MType::Fact(mir::types::FactKind::Binding(b)),
                        );
                        self.inst(Opcode::LoadGName(b), vec![fact], Some(MType::VAL_TOP))
                    }
                    _ => self.js(Opcode::JsGetName(a), vec![], MType::VAL_TOP),
                };
                self.gname_vals.insert(r, a);
                self.push(r, Ty::Val(TagSet::ALL));
                if let Some(&claim) = self.s.gname_types.get(&index) {
                    self.guard_result(claim, pc + op.len(), true);
                }
            }
            GetProp => {
                let a = self.atom(p.next_uint32().unwrap())?;
                let recv = self.pop();
                self.get_prop_op(pc, pc + op.len(), a, recv)?;
            }
            SetProp | StrictSetProp => {
                let a = self.atom(p.next_uint32().unwrap())?;
                let v = self.pop();
                let recv = self.pop();
                self.set_prop_op(pc, pc + op.len(), a, recv, v, op == StrictSetProp);
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
}
