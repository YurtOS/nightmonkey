//! op calls: the lowering of one run of arms of `Run::op`, split out of `build.rs` unchanged.

use super::*;

impl<'s, 'a> Run<'s, 'a> {
    pub(super) fn op_calls(&mut self, op: JSOp, pc: Pc, p: &mut BytecodeParser<'a>) -> R<bool> {
        use JSOp::*;
        match op {
            CallIter | CallContentIter => {
                // An iterator method (`obj[Symbol.iterator]()`, a
                // `return`): the ordinary call, whose uncallable callee
                // throws the iterator protocol's error.
                let argc = usize::from(p.next_uint16().unwrap());
                self.publish_this();
                let n = self.st.len();
                let operands: Vec<Slot> = self.st.drain(n - argc - 2..).collect();
                let vals: Vec<mir::Value> = operands.into_iter().map(|x| self.boxed(x)).collect();
                let r = self.js(Opcode::CallIter, vals, MType::VAL_TOP);
                self.push(r, Ty::Val(TagSet::ALL));
                let claim = self.s.ctx.facts.call_types.get(&self.site(pc)).copied();
                self.guard_result(claim.unwrap_or_default(), pc + op.len(), false);
            }
            SpreadCall | SpreadNew | SpreadSuperCall => {
                // `f(...a)` / `new f(...a)` / `super(...a)`: baseline's
                // helper, over the spread's argument array.
                let construct = op != SpreadCall;
                self.publish_this();
                let nt = if construct {
                    let x = self.pop();
                    self.boxed(x)
                } else {
                    self.const_val(ConstVal::Null)
                };
                let arr = self.pop();
                let thisv = self.pop();
                let callee = self.pop();
                let vals = vec![self.boxed(callee), self.boxed(thisv), self.boxed(arr), nt];
                let (out, ty) = if construct {
                    (MType::val(TagSet::OBJECT), TagSet::OBJECT)
                } else {
                    (MType::VAL_TOP, TagSet::ALL)
                };
                let r = self.js(
                    Opcode::JsRt(RtOp::SpreadCall(u32::from(construct))),
                    vals,
                    out,
                );
                self.push(r, Ty::Val(ty));
            }
            OptimizeSpreadCall => {
                let x = self.pop();
                let v = self.boxed(x);
                let r = self.js(
                    Opcode::JsRt(RtOp::OptimizeSpreadCall),
                    vec![v],
                    MType::VAL_TOP,
                );
                self.push(r, Ty::Val(TagSet::ALL));
            }
            OptimizeGetIterator => {
                let x = self.pop();
                let v = self.boxed(x);
                let b = self.inst(Opcode::IterOptimizable, vec![v], Some(MType::Bool));
                self.push(b, Ty::Bool);
            }
            CheckIsObj => {
                let kind = u32::from(p.next_uint8().unwrap());
                let x = self.top();
                let v = self.boxed(x);
                self.js_void(Opcode::JsRt(RtOp::CheckIsObj(kind)), vec![v]);
            }
            CloseIter => {
                let kind = u32::from(p.next_uint8().unwrap());
                let x = self.pop();
                let v = self.boxed(x);
                self.js_void(Opcode::JsRt(RtOp::CloseIter(kind)), vec![v]);
            }
            Call | CallIgnoresRv | CallContent => {
                let argc = usize::from(p.next_uint16().unwrap());
                if self.math_call(argc) {
                    return Ok(true);
                }
                self.publish_this();
                let n = self.st.len();
                let mut operands: Vec<Slot> = self.st.drain(n - argc - 2..).collect();
                // A receiver (or a `.call`'s `this`) a fence demoted from
                // under construction, for callees to inline: back to its
                // construction state (§2.3).
                {
                    let site = self.site(pc);
                    let facts = &self.s.ctx.facts;
                    let direct = !facts.scripted_targets(site).is_empty();
                    let fwd = (argc >= 1
                        && facts.apply_sites.get(&site) == Some(&crate::facts::CallForm::Call)
                        && (facts.apply_targets.contains_key(&site)
                            || facts.apply_target_sets.contains_key(&site)))
                        || (argc == 2
                            && self.s.apply_fwd.as_ref().is_some_and(|f| f.contains(&pc)));
                    for (i, want) in [(1, direct), (2, fwd)] {
                        if want {
                            if let Some(s) = self.ctor_reguard(operands[i]) {
                                operands[i] = s;
                            }
                        }
                    }
                }
                // A known closure is the callee (context-sensitive: known
                // here, maybe not where the analysis looked); the closures
                // the call passes go to the callee's build.
                let known = match operands[0].ty {
                    Ty::Fn(k) => Some(k),
                    _ => None,
                };
                let fns: Vec<Option<ScriptId>> = operands[2..]
                    .iter()
                    .map(|x| match x.ty {
                        Ty::Fn(k) => Some(k),
                        _ => None,
                    })
                    .collect();
                // An object under construction as the receiver, or as a
                // `.call`'s `this`: callees built for it (§2.3).
                let this_ctor = |x: Option<&Slot>| match x.map(|x| x.ty) {
                    Some(Ty::Ctor(k, n, t)) => Some((k, n, t)),
                    _ => None,
                };
                let recv_ctor = this_ctor(operands.get(1)).map(|c| (c, operands[1]));
                let fwd_ctor = this_ctor(operands.get(2)).map(|c| (c, operands[2]));
                // A published receiver of a proven type: callees built for
                // it, its own guard subsumed.
                let recv_obj = match operands.get(1).map(|x| x.ty) {
                    Some(Ty::Obj(keys, t, sl)) if recv_ctor.is_none() => {
                        Some(((keys.lo.get(), keys.hi.get(), t, sl), operands[1].v))
                    }
                    _ => None,
                };
                let vals: Vec<mir::Value> = operands.into_iter().map(|x| self.boxed(x)).collect();
                let facts_sids = self.s.ctx.facts.scripted_targets(self.site(pc));
                let known_sids: Vec<ScriptId> = known.into_iter().collect();
                let sids: &[ScriptId] = if known.is_some() {
                    &known_sids
                } else {
                    facts_sids
                };
                let sids: Vec<ScriptId> = sids.to_vec();
                let forward = if argc == 2 && self.hasown_site() {
                    Some(self.hasown_call(&vals))
                } else if argc >= 1 {
                    self.call_forward(&vals, &fns[1..], fwd_ctor.map(|(c, x)| (c, x, vals[2])))
                } else {
                    None
                };
                let r = if let Some(r) = forward {
                    r
                } else if argc == 2 && self.s.apply_fwd.as_ref().is_some_and(|f| f.contains(&pc)) {
                    self.apply_forward(&vals, fwd_ctor)
                } else if let Some(targets) = {
                    let rc = recv_ctor.map(|(c, _)| c);
                    let ro = recv_obj.map(|(o, _)| o);
                    let fns = &fns;
                    self.admit(&sids, false, &|s, k, ictx| {
                        s.callee_ctx(k, fns, rc, ro, ictx)
                    })
                } {
                    let raw = recv_ctor.map(|(_, x)| self.ctor_pass(x, vals[1]));
                    let r = if let Some((_, o)) = recv_obj {
                        self.inline_call_this(&targets, &vals, None, Some(o), false)
                    } else {
                        self.inline_call_this(&targets, &vals, None, raw, raw.is_some())
                    };
                    if raw.is_some() {
                        self.ctor_after(&targets, vals[1]);
                    }
                    r
                } else {
                    // Not inlined: a known single callee is called
                    // directly while it is the callee.
                    self.likely_targets = if sids.len() <= MAX_DIRECT_TARGETS
                        && sids.iter().all(|&k| self.s.direct_ok(k))
                    {
                        sids.to_vec()
                    } else {
                        vec![]
                    };
                    let r = self.js(Opcode::Call, vals, MType::VAL_TOP);
                    self.likely_targets.clear();
                    r
                };
                self.push(r, Ty::Val(TagSet::ALL));
                let claim = self.s.ctx.facts.call_types.get(&self.site(pc)).copied();
                self.guard_result(claim.unwrap_or_default(), pc + op.len(), false);
            }

            op => return Err(format!("{op:?}")),
        }
        Ok(true)
    }
}
