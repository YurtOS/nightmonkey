//! op elems: the lowering of one run of arms of `Run::op`, split out of `build.rs` unchanged.

use super::*;

impl<'s, 'a> Run<'s, 'a> {
    pub(super) fn op_elems(&mut self, op: JSOp, pc: Pc, p: &mut BytecodeParser<'a>) -> R<bool> {
        use JSOp::*;
        match op {
            GetElem if self.s.args_reads.as_ref().is_some_and(|r| r.contains(&pc)) => {
                // An element of the elided arguments object: an int32 index
                // within the actual count reads that actual; anything else
                // exits here, where the object is made (`exit_operands`)
                // and baseline reads it.
                let key = *self.st.last().unwrap();
                let ki = if key.ty == Ty::I32 {
                    key.v
                } else {
                    let b = self.boxed(key);
                    self.guard(Opcode::GuardUnbox(UnboxKind::I32), vec![b], MType::I32_TOP)
                };
                let n = self.inst(
                    Opcode::ArgsLength,
                    vec![],
                    Some(MType::i32_range(0, i64::from(i32::MAX))),
                );
                let zero = self.const_i32(0);
                let exit = self.exit_block(false);
                let nonneg = self.inst(
                    Opcode::Cmp(NumRepr::I32, Cc::Ge),
                    vec![ki, zero],
                    Some(MType::Bool),
                );
                let (b1, b2) = (self.new_block(), self.new_block());
                self.term(
                    Opcode::Br,
                    vec![nonneg],
                    vec![Self::goto(b1), Self::goto(exit)],
                );
                self.at(b1);
                let below = self.inst(
                    Opcode::Cmp(NumRepr::I32, Cc::Lt),
                    vec![ki, n],
                    Some(MType::Bool),
                );
                self.term(
                    Opcode::Br,
                    vec![below],
                    vec![Self::goto(b2), Self::goto(exit)],
                );
                self.at(b2);
                self.pop();
                self.pop();
                let v = self.inst(Opcode::ActualArg, vec![ki], Some(MType::VAL_TOP));
                self.push(v, Ty::Val(TagSet::ALL));
            }
            GetElem
                if self
                    .st
                    .last()
                    .and_then(|k| self.const_key_atom(*k))
                    .is_some() =>
            {
                // `o["name"]` (a key MIR knows is that string): `o.name`.
                let key = self.pop();
                let a = self.const_key_atom(key).unwrap();
                let recv = self.pop();
                self.get_prop_op(pc, pc + op.len(), a, recv)?;
            }
            GetElem => {
                self.ta_poly_site = self.s.ctx.facts.elem_poly_sites.contains(&self.site(pc));
                let key = self.pop();
                let recv = self.pop();
                let ta = self.ta_elem(pc, recv, key);
                let r = if let Some((o, i, k)) = ta {
                    // The element inline, as a boxed number; out of
                    // bounds, the generic op (a fence unless it keeps, or
                    // a data read).
                    let (ok, generic, join) =
                        (self.new_block(), self.new_block(), self.new_block());
                    let raw = if k.is_float() {
                        MType::F64_TOP
                    } else {
                        MType::I32_TOP
                    };
                    let p = self.f.add_param(ok, raw);
                    let jr = self.f.add_param(join, MType::VAL_TOP);
                    self.term(
                        Opcode::LoadTa,
                        vec![o, i],
                        vec![
                            Edge {
                                block: ok,
                                args: vec![EdgeArg::Out(0)],
                            },
                            Self::goto(generic),
                        ],
                    );
                    self.at(ok);
                    let ty = if k.is_float() { Ty::F64 } else { Ty::I32 };
                    let b = self.boxed(Slot { v: p, ty });
                    let b = self.weaken(b, MType::VAL_TOP);
                    self.term(
                        Opcode::Jump,
                        vec![],
                        vec![Edge {
                            block: join,
                            args: vec![EdgeArg::Value(b)],
                        }],
                    );
                    self.at(generic);
                    let (x, kb) = (self.boxed(recv), self.boxed(key));
                    let r = self.get_elem(x, kb);
                    self.term(
                        Opcode::Jump,
                        vec![],
                        vec![Edge {
                            block: join,
                            args: vec![EdgeArg::Value(r)],
                        }],
                    );
                    self.at(join);
                    Some(jr)
                } else {
                    None
                };
                let r = if let Some(r) = r {
                    r
                } else {
                    // A dense element load's miss (a hole, out of bounds) exits
                    // rather than running the generic read and rejoining, where no
                    // typed array can reach the site (`elem_poly_sites`): a native
                    // receiver is then one with dense elements and a miss is rare.
                    // Where one can, the generic read stays: a typed array the
                    // analysis took for an array misses every time, and an exit per
                    // execution costs far more than the facts the rejoin kills
                    // (pdfjs's FlateStream_getCode: 1.5M exits).
                    match self.native_elem(pc, recv, key) {
                        Some((o, i)) if !self.ta_poly_site => {
                            // A dense element inline; out of bounds or a
                            // hole, an exit (nothing has happened yet): no
                            // generic op rejoins, so what the code after it
                            // knows about memory survives it.
                            self.guard(Opcode::LoadElem, vec![o, i], MType::VAL_TOP)
                        }
                        Some((o, i)) => {
                            // A dense element inline; out of bounds or a
                            // hole, the generic op (a fence unless it keeps,
                            // or a data read).
                            let (ok, generic, join) =
                                (self.new_block(), self.new_block(), self.new_block());
                            let p = self.f.add_param(ok, MType::VAL_TOP);
                            let jr = self.f.add_param(join, MType::VAL_TOP);
                            self.term(
                                Opcode::LoadElem,
                                vec![o, i],
                                vec![
                                    Edge {
                                        block: ok,
                                        args: vec![EdgeArg::Out(0)],
                                    },
                                    Self::goto(generic),
                                ],
                            );
                            self.at(ok);
                            self.term(
                                Opcode::Jump,
                                vec![],
                                vec![Edge {
                                    block: join,
                                    args: vec![EdgeArg::Value(p)],
                                }],
                            );
                            self.at(generic);
                            let (x, k) = (self.boxed(recv), self.boxed(key));
                            let r = self.get_elem(x, k);
                            self.term(
                                Opcode::Jump,
                                vec![],
                                vec![Edge {
                                    block: join,
                                    args: vec![EdgeArg::Value(r)],
                                }],
                            );
                            self.at(join);
                            jr
                        }
                        None => {
                            let (x, k) = (self.boxed(recv), self.boxed(key));
                            self.get_elem(x, k)
                        }
                    }
                };
                self.ta_poly_site = false;
                self.push(r, Ty::Val(TagSet::ALL));
                let claim = self.s.ctx.facts.elem_sites.get(&self.site(pc)).copied();
                self.guard_result(claim.unwrap_or_default(), pc + op.len(), true);
            }
            SetElem | StrictSetElem
                if self.st.len() >= 2
                    && self.const_key_atom(self.st[self.st.len() - 2]).is_some() =>
            {
                // `o["name"] = v`: `o.name = v`.
                let v = self.pop();
                let key = self.pop();
                let a = self.const_key_atom(key).unwrap();
                let recv = self.pop();
                self.set_prop_op(pc, pc + op.len(), a, recv, v, op == StrictSetElem);
            }
            SetElem | StrictSetElem => {
                self.ta_poly_site = self.s.ctx.facts.elem_poly_sites.contains(&self.site(pc));
                let v = self.pop();
                let key = self.pop();
                let recv = self.pop();
                let duty = self.ranges_duty(pc, v);
                // The value as the kind stores it, raw: an int32 for an
                // integer kind (Uint8Clamped clamps it), a number for a
                // float one. A value of unknown type is unboxed at run
                // time; a miss (or a
                // double into an integer kind) takes the generic op.
                let ta = self.ta_elem(pc, recv, key).and_then(|(o, i, k)| {
                    match (k.is_float(), v.ty.num()) {
                        (true, Some(_)) => Some((o, i, k, Some(self.as_f64(v)))),
                        (false, Some(Num::I32)) => Some((o, i, k, Some(self.as_i32(v)))),
                        (_, None) if !matches!(v.ty, Ty::Dead) => Some((o, i, k, None)),
                        _ => None,
                    }
                });
                if let Some((o, i, k, raw)) = ta {
                    let v = self.demote_value_for_generic_arm(v);
                    let (ok, generic, join) =
                        (self.new_block(), self.new_block(), self.new_block());
                    let raw = match raw {
                        Some(r) => r,
                        None => {
                            let (kind, ty) = if k.is_float() {
                                (UnboxKind::F64Num, MType::F64_TOP)
                            } else {
                                (UnboxKind::I32, MType::I32_TOP)
                            };
                            let unboxed = self.new_block();
                            let r = self.f.add_param(unboxed, ty);
                            let y = self.boxed(v);
                            self.term(
                                Opcode::GuardUnbox(kind),
                                vec![y],
                                vec![
                                    Edge {
                                        block: unboxed,
                                        args: vec![EdgeArg::Out(0)],
                                    },
                                    Self::goto(generic),
                                ],
                            );
                            self.at(unboxed);
                            r
                        }
                    };
                    self.term(
                        Opcode::StoreTa,
                        vec![o, i, raw],
                        vec![Self::goto(ok), Self::goto(generic)],
                    );
                    self.at(ok);
                    self.term(Opcode::Jump, vec![], vec![Self::goto(join)]);
                    self.at(generic);
                    let (x, kb, y) = (self.boxed(recv), self.boxed(key), self.boxed(v));
                    self.set_elem(op == StrictSetElem, duty, x, kb, y, v);
                    self.term(Opcode::Jump, vec![], vec![Self::goto(join)]);
                    self.at(join);
                    self.st.push(v);
                } else {
                    match self.native_elem(pc, recv, key) {
                        Some((o, i)) if !self.ta_poly_site => {
                            // A dense overwrite or append inline (the lowering's
                            // append arm); a store it cannot do (growth, a hole
                            // past the prototypes' proof) exits before it.
                            let y = if v.ty == Ty::F64
                                && self
                                    .s
                                    .ctx
                                    .facts
                                    .elem_write_sites
                                    .get(&self.site(pc))
                                    .is_some_and(|m| {
                                        m.prims().intersects(crate::opsem::PRIM_DOUBLE)
                                    }) {
                                let t = MType::val(TagSet::DOUBLE);
                                self.inst(Opcode::BoxDouble, vec![v.v], Some(t))
                            } else {
                                self.boxed(v)
                            };
                            let ok = self.new_block();
                            let fail = self.exit_block(false);
                            self.term(
                                Opcode::StoreElem(duty, true),
                                vec![o, i, y],
                                vec![Self::goto(ok), Self::goto(fail)],
                            );
                            self.at(ok);
                            self.st.push(v);
                        }
                        Some((o, i)) => {
                            // A dense overwrite inline; else the generic op.
                            let v = self.demote_value_for_generic_arm(v);
                            // A double under a write claim that admits Double
                            // goes in as the double it is: every
                            // read of the element admits the tag.
                            let y = if v.ty == Ty::F64
                                && self
                                    .s
                                    .ctx
                                    .facts
                                    .elem_write_sites
                                    .get(&self.site(pc))
                                    .is_some_and(|m| {
                                        m.prims().intersects(crate::opsem::PRIM_DOUBLE)
                                    }) {
                                let t = MType::val(TagSet::DOUBLE);
                                self.inst(Opcode::BoxDouble, vec![v.v], Some(t))
                            } else {
                                self.boxed(v)
                            };
                            let (ok, generic, join) =
                                (self.new_block(), self.new_block(), self.new_block());
                            self.term(
                                Opcode::StoreElem(duty, false),
                                vec![o, i, y],
                                vec![Self::goto(ok), Self::goto(generic)],
                            );
                            self.at(ok);
                            self.term(Opcode::Jump, vec![], vec![Self::goto(join)]);
                            self.at(generic);
                            let (x, k) = (self.boxed(recv), self.boxed(key));
                            self.set_elem(op == StrictSetElem, duty, x, k, y, v);
                            self.term(Opcode::Jump, vec![], vec![Self::goto(join)]);
                            self.at(join);
                            self.st.push(v);
                        }
                        None => {
                            let (x, k, y) = (self.boxed(recv), self.boxed(key), self.boxed(v));
                            self.set_elem(op == StrictSetElem, duty, x, k, y, v);
                            self.repush(v);
                        }
                    }
                }
            }
            // Generic operations through their runtime helpers.
            Instanceof | In | HasOwn => {
                let b = self.pop();
                let a = self.pop();
                let (x, y) = (self.boxed(a), self.boxed(b));
                let r = match op {
                    Instanceof => RtOp::Instanceof,
                    In => RtOp::In,
                    _ => RtOp::HasOwn,
                };
                let v = self.js(Opcode::JsRt(r), vec![x, y], MType::val(TagSet::BOOLEAN));
                self.push(v, Ty::Val(TagSet::BOOLEAN));
                if op == Instanceof {
                    self.narrow_instanceof(a);
                }
            }
            DelProp | StrictDelProp => {
                let a = self.atom(p.next_uint32().unwrap())?;
                let o = self.pop();
                let x = self.boxed(o);
                let r = RtOp::DelProp(a, op == StrictDelProp);
                let v = self.js(Opcode::JsRt(r), vec![x], MType::val(TagSet::BOOLEAN));
                self.push(v, Ty::Val(TagSet::BOOLEAN));
            }
            DelElem | StrictDelElem => {
                let k = self.pop();
                let o = self.pop();
                let (x, y) = (self.boxed(o), self.boxed(k));
                let r = RtOp::DelElem(op == StrictDelElem);
                let v = self.js(Opcode::JsRt(r), vec![x, y], MType::val(TagSet::BOOLEAN));
                self.push(v, Ty::Val(TagSet::BOOLEAN));
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
}
