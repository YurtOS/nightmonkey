//! op gen: the lowering of one run of arms of `Run::op`, split out of `build.rs` unchanged.

use super::*;

impl<'s, 'a> Run<'s, 'a> {
    pub(super) fn op_gen(&mut self, op: JSOp, pc: Pc, p: &mut BytecodeParser<'a>) -> R<bool> {
        use JSOp::*;
        match op {
            CheckThisReinit => {
                let x = self.top();
                let v = self.boxed(x);
                self.js_void(
                    Opcode::JsRt(RtOp::Check(mir::ops::CHECK_THIS_REINIT)),
                    vec![v],
                );
            }
            CheckReturn => {
                // [this] -> [result], with the frame's rval.
                let t = self.pop();
                let rv = self.st[self.rval_ix()];
                let (x, y) = (self.boxed(t), self.boxed(rv));
                let r = self.js(Opcode::JsRt(RtOp::CheckReturn), vec![x, y], MType::VAL_TOP);
                self.push(r, Ty::Val(TagSet::ALL));
            }
            AddDisposable => {
                let hint = u32::from(p.next_uint8().unwrap());
                let nc = self.pop();
                let m = self.pop();
                let v = self.pop();
                let vals = vec![self.boxed(v), self.boxed(m), self.boxed(nc)];
                self.js_void(Opcode::JsRt(RtOp::AddDisposable(hint)), vals);
            }
            TakeDisposeCapability => {
                let r = self.js(
                    Opcode::JsRt(RtOp::TakeDisposeCapability),
                    vec![],
                    MType::VAL_TOP,
                );
                self.push(r, Ty::Val(TagSet::ALL));
            }
            CreateSuppressedError => {
                let sup = self.pop();
                let e = self.pop();
                let vals = vec![self.boxed(e), self.boxed(sup)];
                let r = self.js(
                    Opcode::JsRt(RtOp::CreateSuppressedError),
                    vals,
                    MType::val(TagSet::OBJECT),
                );
                self.push(r, Ty::Val(TagSet::OBJECT));
            }
            GetBoundName => {
                let a = self.atom(p.next_uint32().unwrap())?;
                let x = self.pop();
                let v = self.boxed(x);
                let r = self.js(Opcode::JsRt(RtOp::GetBoundName(a)), vec![v], MType::VAL_TOP);
                self.push(r, Ty::Val(TagSet::ALL));
            }
            ObjWithProto => {
                let x = self.pop();
                let v = self.boxed(x);
                let r = self.js(
                    Opcode::JsRt(RtOp::ObjWithProto),
                    vec![v],
                    MType::val(TagSet::OBJECT),
                );
                self.push(r, Ty::Val(TagSet::OBJECT));
            }
            NewPrivateName => {
                let a = self.atom(p.next_uint32().unwrap())?;
                let t = TagSet::prims(crate::opsem::PRIM_SYMBOL);
                let r = self.js(Opcode::JsRt(RtOp::NewPrivateName(a)), vec![], MType::val(t));
                self.push(r, Ty::Val(t));
            }
            DynamicImport => {
                #[cfg(feature = "ff159")]
                {
                    let phase = p.next_uint8().unwrap();
                    if phase != crate::wasm::translate::IMPORT_PHASE_EVALUATION {
                        return Err(format!("DynamicImport phase {phase}"));
                    }
                }
                let o = self.pop();
                let s = self.pop();
                let vals = vec![self.boxed(s), self.boxed(o)];
                let r = self.js(
                    Opcode::JsRt(RtOp::DynamicImport),
                    vals,
                    MType::val(TagSet::OBJECT),
                );
                self.push(r, Ty::Val(TagSet::OBJECT));
            }
            SpreadEval | StrictSpreadEval => {
                self.publish_this();
                let arr = self.pop();
                let thisv = self.pop();
                let callee = self.pop();
                let vals = vec![self.boxed(callee), self.boxed(thisv), self.boxed(arr)];
                let r = self.js(
                    Opcode::JsRt(RtOp::SpreadEval(pc.get())),
                    vals,
                    MType::VAL_TOP,
                );
                self.push(r, Ty::Val(TagSet::ALL));
            }
            Generator => {
                let r = self.js(
                    Opcode::JsRt(RtOp::CreateGenerator),
                    vec![],
                    MType::val(TagSet::OBJECT),
                );
                self.push(r, Ty::Val(TagSet::OBJECT));
            }
            InitialYield | Yield | Await => {
                // Suspend with the frame as an exit writes it; a resume
                // enters this yield's own root at the landing (§5.2's
                // onramps, keyed by the resume index).
                let index = p.next_uint24().unwrap();
                let initial = op == InitialYield;
                let before = self.st.clone();
                let ops = self.exit_operands(pc, &before);
                let (nargs, nlocals) = (self.s.nargs, self.s.nlocals);
                self.term(
                    Opcode::GenSuspend {
                        pc,
                        index,
                        nargs,
                        nlocals,
                        initial,
                    },
                    ops,
                    vec![],
                );
                let popped = if initial { 1 } else { 2 };
                self.resume_root(index, pc + op.len(), &before, popped)?;
            }
            AfterYield => {
                p.next_uint24();
            }
            FinalYieldRval => {
                let g = self.pop();
                let gv = self.boxed(g);
                self.js_void(Opcode::JsRt(RtOp::GenFinal), vec![gv]);
                let x = self.st[self.rval_ix()];
                let v = if x.ty == Ty::Dead {
                    self.const_val(ConstVal::Undefined)
                } else {
                    self.boxed(x)
                };
                self.term(Opcode::Return, vec![v], vec![]);
            }
            ResumeKind => {
                let kind = i32::from(p.next_uint8().unwrap());
                let v = self.const_i32(kind);
                self.push(v, Ty::I32);
            }
            #[cfg(feature = "ff147")]
            IsGenClosing => {
                let x = self.top();
                let v = self.boxed(x);
                let b = self.inst(Opcode::IsGenClosing, vec![v], Some(MType::Bool));
                self.push(b, Ty::Bool);
            }
            #[cfg(feature = "ff147")]
            CheckResumeKind => {
                // [val, gen, kind] -> [val]: next goes on; throw and return
                // raise through the helper, which always fails.
                let k = self.pop();
                let g = self.pop();
                let next = match k.ty {
                    Ty::I32 => self.const_i32_of(k.v),
                    _ => None,
                };
                if next != Some(0) {
                    let ki = if k.ty == Ty::I32 {
                        k.v
                    } else {
                        let kv = self.boxed(k);
                        self.guard(Opcode::GuardUnbox(UnboxKind::I32), vec![kv], MType::I32_TOP)
                    };
                    let zero = self.const_i32(0);
                    let is_next = self.inst(
                        Opcode::Cmp(NumRepr::I32, Cc::Eq),
                        vec![ki, zero],
                        Some(MType::Bool),
                    );
                    let (cont, raise) = (self.new_block(), self.new_block());
                    self.term(
                        Opcode::Br,
                        vec![is_next],
                        vec![Self::goto(cont), Self::goto(raise)],
                    );
                    self.at(raise);
                    let v = self.top();
                    let vals = vec![self.boxed(v), self.boxed(g), self.boxed(k)];
                    // A return stages its value as the frame's rval: the
                    // throw's exit must leave that slot as the helper wrote
                    // it.
                    let ix = self.rval_ix();
                    self.pre[ix] = Slot::dead();
                    self.js_void(Opcode::JsRt(RtOp::GenCheckResume), vals);
                    self.term(Opcode::Unreachable, vec![], vec![]);
                    self.at(cont);
                }
            }
            AsyncAwait | AsyncResolve => {
                let g = self.pop();
                let v = self.pop();
                let vals = vec![self.boxed(v), self.boxed(g)];
                let r = self.js(
                    Opcode::JsRt(RtOp::AsyncAwait(u32::from(op == AsyncResolve))),
                    vals,
                    MType::VAL_TOP,
                );
                self.push(r, Ty::Val(TagSet::ALL));
            }
            AsyncReject => {
                let g = self.pop();
                let stack = self.pop();
                let reason = self.pop();
                let vals = vec![self.boxed(reason), self.boxed(stack), self.boxed(g)];
                let r = self.js(Opcode::JsRt(RtOp::AsyncReject), vals, MType::VAL_TOP);
                self.push(r, Ty::Val(TagSet::ALL));
            }
            CanSkipAwait => {
                let x = self.top();
                let v = self.boxed(x);
                let r = self.js(
                    Opcode::JsRt(RtOp::CanSkipAwait),
                    vec![v],
                    MType::val(TagSet::BOOLEAN),
                );
                self.push(r, Ty::Val(TagSet::BOOLEAN));
            }
            MaybeExtractAwaitValue => {
                // [v, can] -> [v', can]
                let can = self.pop();
                let x = self.pop();
                let vals = vec![self.boxed(x), self.boxed(can)];
                let r = self.js(Opcode::JsRt(RtOp::MaybeExtractAwait), vals, MType::VAL_TOP);
                self.push(r, Ty::Val(TagSet::ALL));
                self.repush(can);
            }
            Resume => {
                let k = self.pop();
                let v = self.pop();
                let g = self.pop();
                let vals = vec![self.boxed(g), self.boxed(v), self.boxed(k)];
                let r = self.js(Opcode::JsRt(RtOp::Resume), vals, MType::VAL_TOP);
                self.push(r, Ty::Val(TagSet::ALL));
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
}
