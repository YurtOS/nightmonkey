//! op control: the lowering of one run of arms of `Run::op`, split out of `build.rs` unchanged.

use super::*;

impl<'s, 'a> Run<'s, 'a> {
    pub(super) fn op_control(&mut self, op: JSOp, pc: Pc, p: &mut BytecodeParser<'a>) -> R<bool> {
        use JSOp::*;
        match op {
            // --- control ---
            Goto => {
                let off = p.next_int32().unwrap();
                self.jump_to(pc.branch(off), Some(pc));
            }
            JumpIfFalse | JumpIfTrue => {
                let off = p.next_int32().unwrap();
                let c = self.pop();
                let t = self.truthy(c);
                let (target, next) = (pc.branch(off), pc + op.len());
                if op == JumpIfTrue {
                    self.branch(t, target, next);
                } else {
                    self.branch(t, next, target);
                }
            }
            And | Or => {
                let off = p.next_int32().unwrap();
                let c = self.top();
                let t = self.truthy(c);
                let (target, next) = (pc.branch(off), pc + op.len());
                if op == Or {
                    self.branch(t, target, next);
                } else {
                    self.branch(t, next, target);
                }
            }
            Return => {
                let x = self.pop();
                let v = self.boxed(x);
                self.note_this_out();
                self.ctor_stamp();
                self.return_restamps();
                self.term(Opcode::Return, vec![v], vec![]);
            }
            RetRval => {
                let x = self.st[self.rval_ix()];
                let v = self.boxed(x);
                self.note_this_out();
                self.ctor_stamp();
                self.return_restamps();
                self.term(Opcode::Return, vec![v], vec![]);
            }

            FunctionThis => {
                let x = self.st[0];
                if self.s.script.strict || x.ty.tags().is_nonempty_subset_of(TagSet::OBJECT) {
                    self.st.push(x);
                } else {
                    // Sloppy: an object `this` is itself; anything else is
                    // boxed (null and undefined become the global `this`).
                    self.demote_for_generic_arm(1);
                    let obj = TagSet::OBJECT;
                    let (t, e, j) = (self.new_block(), self.new_block(), self.new_block());
                    let p = self.f.add_param(t, MType::val(obj));
                    let r = self.f.add_param(j, MType::val(obj));
                    let v = self.boxed(x);
                    self.term(
                        Opcode::GuardTags(obj),
                        vec![v],
                        vec![
                            Edge {
                                block: t,
                                args: vec![EdgeArg::Out(0)],
                            },
                            Self::goto(e),
                        ],
                    );
                    self.at(t);
                    self.term(
                        Opcode::Jump,
                        vec![],
                        vec![Edge {
                            block: j,
                            args: vec![EdgeArg::Value(p)],
                        }],
                    );
                    self.at(e);
                    let b = self.js_static(Opcode::JsBoxThis, vec![v], MType::val(obj));
                    self.term(
                        Opcode::Jump,
                        vec![],
                        vec![Edge {
                            block: j,
                            args: vec![EdgeArg::Value(b)],
                        }],
                    );
                    self.at(j);
                    self.push(r, Ty::Val(obj));
                }
                self.guard_this_layout();
            }
            String => {
                let a = self.atom(p.next_uint32().unwrap())?;
                let s = self.inst(
                    Opcode::ConstStr(a),
                    vec![],
                    Some(MType::Str(mir::types::StrInfo { atom: Some(a) })),
                );
                let x = Slot {
                    v: s,
                    ty: Ty::Val(TagSet::STRING),
                };
                let t = mir::ops::box_type(&self.f.ty(s)).expect("strings box");
                let v = self.inst(Opcode::Box, vec![x.v], Some(t));
                let v = self.weaken(v, MType::val(TagSet::STRING));
                self.push(v, Ty::Val(TagSet::STRING));
            }
            BindUnqualifiedGName => {
                let index = p.next_uint32().unwrap();
                let a = self.atom(index)?;
                if let Some(&slot) = self.s.syn_bids.get(&index) {
                    // The target of an assignment to a syntactic global:
                    // while the binding is the global's writable data
                    // property (the check the store makes, which then
                    // folds into this one), the global object; else exit
                    // here.
                    let b = self.binding(a, slot);
                    self.guard(
                        Opcode::CheckBinding(b, BindingCheck::Write),
                        vec![],
                        MType::Fact(mir::types::FactKind::Binding(b)),
                    );
                    let r = self.inst(
                        Opcode::GlobalObject,
                        vec![],
                        Some(MType::val(TagSet::OBJECT)),
                    );
                    self.push(r, Ty::Val(TagSet::OBJECT));
                    return Ok(true);
                }
                let r = self.js(Opcode::JsBindGName(a), vec![], MType::val(TagSet::OBJECT));
                self.push(r, Ty::Val(TagSet::OBJECT));
            }
            SetGName | StrictSetGName => {
                let index = p.next_uint32().unwrap();
                let a = self.atom(index)?;
                let v = self.pop();
                let env = self.pop();
                let (e, y) = (self.boxed(env), self.boxed(v));
                match self.s.syn_bids.get(&index) {
                    Some(&slot) => {
                        // §3: `check.binding.write`, then a leaf store of
                        // the slot; a miss exits here.
                        let b = self.binding(a, slot);
                        let fact = self.guard(
                            Opcode::CheckBinding(b, BindingCheck::Write),
                            vec![],
                            MType::Fact(mir::types::FactKind::Binding(b)),
                        );
                        let (t, _) = self.f.add_inst(
                            self.cur,
                            Opcode::StoreGName(b),
                            vec![fact, y],
                            &[],
                            vec![],
                        );
                        self.f.witnesses[t] = Some(mir::func::Witness {
                            may_kill: mir::types::KillPattern::of(mir::ops::store_gname_kills(
                                b, &self.mm,
                            )),
                        });
                    }
                    _ => {
                        self.js_void_keep(Opcode::JsSetName(a, op == StrictSetGName), vec![e, y], v)
                    }
                }
                self.repush(v);
            }
            TableSwitch => {
                let default_off = p.next_int32().unwrap();
                let low = p.next_int32().unwrap();
                let high = p.next_int32().unwrap();
                let first = p.next_uint24().unwrap() as usize;
                let x = self.pop();
                let n = usize::try_from((i64::from(high) - i64::from(low) + 1).max(0)).unwrap();
                let default = pc.branch(default_off);
                let mut targets = vec![];
                for k in 0..n {
                    let t = *self
                        .s
                        .script
                        .resume_offsets
                        .get(first + k)
                        .ok_or("TableSwitch resume index out of range")?;
                    targets.push(t);
                }
                // An int32, or a double that is exactly one, selects a case;
                // anything else takes the default.
                let i = match x.ty {
                    Ty::I32 => x.v,
                    Ty::F64 | Ty::Val(_) | Ty::Fn(_) => {
                        let f = match x.ty {
                            Ty::F64 => x.v,
                            _ => {
                                let d = self.new_block();
                                let o = self.f.add_param(d, MType::F64_TOP);
                                let Some(de) = self.edge_to(default, Some(pc)) else {
                                    self.term(Opcode::Unreachable, vec![], vec![]);
                                    return Ok(true);
                                };
                                self.term(
                                    Opcode::GuardUnbox(UnboxKind::F64Num),
                                    vec![x.v],
                                    vec![
                                        Edge {
                                            block: d,
                                            args: vec![EdgeArg::Out(0)],
                                        },
                                        de,
                                    ],
                                );
                                self.at(d);
                                o
                            }
                        };
                        // -0 selects case 0 as +0 does: `x + 0` is +0 for
                        // either zero and `x` otherwise.
                        let z = self.const_f64(0.0);
                        let f = self.inst(
                            Opcode::F64Arith(F64Op::Add),
                            vec![f, z],
                            Some(MType::F64_TOP),
                        );
                        let d = self.new_block();
                        let o = self.f.add_param(d, MType::I32_TOP);
                        let Some(de) = self.edge_to(default, Some(pc)) else {
                            self.term(Opcode::Unreachable, vec![], vec![]);
                            return Ok(true);
                        };
                        self.term(
                            Opcode::F64ToIntExact,
                            vec![f],
                            vec![
                                Edge {
                                    block: d,
                                    args: vec![EdgeArg::Out(0)],
                                },
                                de,
                            ],
                        );
                        self.at(d);
                        o
                    }
                    Ty::Bool
                    | Ty::Obj(..)
                    | Ty::ObjHint(_)
                    | Ty::Ctor(..)
                    | Ty::Native
                    | Ty::Ta(_) => {
                        self.jump_to(default, Some(pc));
                        return Ok(true);
                    }
                    Ty::Dead => unreachable!("a dead slot is never read"),
                };
                let lo = self.const_i32(low);
                let idx = self.inst(
                    Opcode::I32Wrap(ArithOp::Sub),
                    vec![i, lo],
                    Some(MType::I32_TOP),
                );
                let mut edges = vec![];
                for t in targets.into_iter().chain([default]) {
                    match self.edge_to(t, Some(pc)) {
                        Some(e) => edges.push(e),
                        None => {
                            self.term(Opcode::Unreachable, vec![], vec![]);
                            return Ok(true);
                        }
                    }
                }
                self.term(Opcode::Switch(u32::try_from(n).unwrap()), vec![idx], edges);
            }
            Case => {
                // [lval, cond]: true pops both and jumps; false keeps lval.
                let off = p.next_int32().unwrap();
                let c = self.pop();
                let t = self.truthy(c);
                let (target, next) = (pc.branch(off), pc + op.len());
                let ne = self.edge_to(next, Some(pc));
                let lval = self.pop();
                let te = self.edge_to(target, Some(pc));
                self.st.push(lval);
                match (te, ne) {
                    (Some(te), Some(ne)) => self.term(Opcode::Br, vec![t], vec![te, ne]),
                    _ => self.term(Opcode::Unreachable, vec![], vec![]),
                }
            }
            Default => {
                let off = p.next_int32().unwrap();
                self.pop();
                self.jump_to(pc.branch(off), Some(pc));
            }
            StrictConstantEq | StrictConstantNe => {
                let operand = p.next_uint16().unwrap();
                let a = self.pop();
                // Against undefined or null: a tag test.
                let exact = match (operand >> 8) & 0xFF {
                    3 => Some(TagSet::prims(PRIM_UNDEFINED)),
                    4 => Some(TagSet::prims(PRIM_NULL)),
                    _ => None,
                };
                if exact.is_some_and(|t| self.fuse_test(a, t, op == StrictConstantNe)) {
                    return Ok(true);
                }
                let x = self.boxed(a);
                let mut r = self.inst(
                    Opcode::JsConstantStrictEq(operand),
                    vec![x],
                    Some(MType::Bool),
                );
                if op == StrictConstantNe {
                    r = self.not(r);
                }
                self.push(r, Ty::Bool);
            }
            IsNullOrUndefined => {
                // [v] -> [v, v is null or undefined], decided by the tags
                // when they say, else by a tag test merging both ways.
                let a = self.top();
                let nullish = TagSet::prims(PRIM_NULL | PRIM_UNDEFINED);
                let tags = a.ty.tags();
                let r = if tags.subset_of(nullish) {
                    self.inst(Opcode::ConstBool(true), vec![], Some(MType::Bool))
                } else if tags.intersect(nullish).is_empty() {
                    self.inst(Opcode::ConstBool(false), vec![], Some(MType::Bool))
                } else {
                    if self.fuse_test(a, nullish, false) {
                        return Ok(true);
                    }
                    let x = self.boxed(a);
                    self.tag_test(x, nullish)
                };
                self.push(r, Ty::Bool);
            }
            TypeofEq => {
                let operand = p.next_uint8().unwrap();
                let a = self.pop();
                // A type whose values are exactly some tags (no object is
                // one): a tag test.
                let exact = match operand & 0x0f {
                    3 => Some(TagSet::STRING),
                    4 => Some(TagSet::NUMBER),
                    5 => Some(TagSet::BOOLEAN),
                    6 => Some(TagSet::prims(crate::opsem::PRIM_SYMBOL)),
                    7 => Some(TagSet::prims(PRIM_BIGINT)),
                    _ => None,
                };
                if exact.is_some_and(|t| self.fuse_test(a, t, operand & 0x80 != 0)) {
                    return Ok(true);
                }
                let x = self.boxed(a);
                let r = self.inst(Opcode::JsTypeofEq(operand), vec![x], Some(MType::Bool));
                self.push(r, Ty::Bool);
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
}
