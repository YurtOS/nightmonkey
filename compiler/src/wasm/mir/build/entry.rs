//! entry: methods of `Run`, split out of `build.rs` unchanged.

use super::*;

impl<'s, 'a> Run<'s, 'a> {
    pub(super) fn build(&mut self) -> R<()> {
        self.entry();
        let ops = self.s.ops.clone();
        let mut i = 0;
        while i < ops.len() {
            let Op { pc, op } = ops[i];
            if self.s.leaders.contains(&pc) {
                if self.live {
                    let from = if i > 0 { Some(ops[i - 1].pc) } else { None };
                    self.jump_to(pc, from);
                }
                if let Some(tys) = self.enter(pc) {
                    let b = self.block_for(pc, Some(pc)).unwrap();
                    let mut params = self.f.blocks[b].params.clone().into_iter();
                    self.st = tys
                        .iter()
                        .map(|&ty| match ty {
                            Ty::Dead => Slot::dead(),
                            ty => Slot {
                                v: params.next().unwrap(),
                                ty,
                            },
                        })
                        .collect();
                    self.at(b);
                }
            }
            if self.live {
                if self.s.depths.at(pc)
                    != Some(u32::try_from(self.st.len() - self.frame_len()).unwrap())
                {
                    return Err(format!("BUG: stack depth at {pc} disagrees"));
                }
                self.pc = pc;
                self.f.cur_pc = pc.get() + 1;
                self.pre = self.st.clone();
                self.renames.clear();
                self.exit_blk = None;
                self.throw_blk = None;
                self.op(op)?;
                if self.live {
                    self.local_restamp(pc);
                    if op == JSOp::FunctionThis {
                        let v = self.top().v;
                        self.this_vals.insert(v);
                    }
                    // A property read's value class, hinted.
                    if matches!(op, JSOp::GetProp | JSOp::GetElem) {
                        if let Some(&(lo, hi)) =
                            self.s.ctx.facts.field_cls_sites.get(&self.site(pc))
                        {
                            if lo == hi {
                                let v = self.top().v;
                                self.val_cls.insert(v, lo.get());
                            }
                        }
                    }
                }
            }
            i += 1;
        }
        if self.live {
            self.term(Opcode::Unreachable, vec![], vec![]);
        }
        self.f.cur_pc = 0;
        let headers: Vec<Pc> = self.preheaders.keys().copied().collect();
        for h in headers {
            if self.wants_onramp(h) {
                self.onramp_root(h);
            }
        }
        Ok(())
    }

    /// The onramp-root policy (`docs/BASELINE.md` §7): every outermost
    /// loop gets a root, which needs no code duplication. An inner loop's
    /// root side-enters every loop around it, which waffle's reducifier
    /// pays for by duplicating code, so it gets one only when its
    /// outermost enclosing loop is small.
    pub(super) fn wants_onramp(&self, h: Pc) -> bool {
        let outer = self
            .s
            .loops
            .iter()
            .filter(|&(&hh, &e)| hh < h && h < e)
            .map(|(&hh, &e)| e.get() - hh.get())
            .max();
        match outer {
            None => true,
            // Inlined callees make the loops around it bigger than their
            // bytecode says, and the duplication with them.
            Some(_) if !self.f.inline_frames.is_empty() => false,
            // Every exit of a function holding the elided `arguments`
            // placeholder materialises the object (`exit_operands`), which
            // is most of its code; copying that for a side entry made one
            // such constructor 53x larger (13.7 MB).
            Some(_) if !self.f.frame.elided_args.is_empty() => false,
            Some(len) => len <= self.s.ctx.opts.inner_onramp_bytes(),
        }
    }

    /// The onramp root `O` for loop header `h` (§5.2): the frame at `h`,
    /// all `Val(⊤)`, guarded up to the preheader's param types. On
    /// success it enters the preheader; on failure it re-deopts at `h`
    /// with its own params.
    pub(super) fn onramp_root(&mut self, h: Pc) {
        let tys = self.entry_types[&h].clone();
        let p = self.preheaders[&h];
        let o = self.new_block();
        self.f.roots.push(Root {
            kind: RootKind::Onramp(h),
            block: o,
        });
        let params: Vec<mir::Value> = tys
            .iter()
            .map(|_| self.f.add_param(o, MType::VAL_TOP))
            .collect();
        let depth = tys.len() - self.frame_len();
        self.f.frame.depths.insert(h, u32::try_from(depth).unwrap());
        let fail = self.new_block();
        self.at(fail);
        let (nargs, nlocals) = (self.s.nargs, self.s.nlocals);
        self.term(
            Opcode::Exit {
                pc: h,
                nargs,
                nlocals,
            },
            params.clone(),
            vec![],
        );
        self.at(o);
        let args = self
            .guard_to(&params, &tys, fail)
            .into_iter()
            .map(EdgeArg::Value)
            .collect();
        self.term(Opcode::Jump, vec![], vec![Edge { block: p, args }]);
    }

    /// The `Resume` root for resume index `index` at landing `pc`: the
    /// frame as the resume restored it, all `Val(⊤)`, guarded up to the
    /// types the slots had at the yield (`before`, whose top `popped`
    /// values the yield consumed; the landing's three resume values are
    /// `Val(⊤)`). The walk goes on from there; a miss exits at the landing.
    pub(super) fn resume_root(
        &mut self,
        index: u32,
        pc: Pc,
        before: &[Slot],
        popped: usize,
    ) -> R<()> {
        let fl = self.frame_len();
        let depth = self
            .s
            .depths
            .at(pc)
            .ok_or("resume landing without a stack depth")? as usize;
        let saved = before.len() - fl - popped;
        if depth != saved + 3 {
            return Err(format!(
                "BUG: resume landing {pc} at depth {depth}, not {}",
                saved + 3
            ));
        }
        let live = self.s.live.get(&pc).cloned();
        let mut tys: Vec<Ty> = before[..fl + saved].iter().map(|x| x.ty).collect();
        for (i, t) in tys.iter_mut().enumerate().take(fl) {
            if live.as_ref().is_some_and(|l| i < l.len() && !l[i]) {
                *t = Ty::Dead;
            }
        }
        tys.extend([Ty::Val(TagSet::ALL); 3]);
        let o = self.new_block();
        self.f.roots.push(Root {
            kind: RootKind::Resume { index, pc },
            block: o,
        });
        let params: Vec<mir::Value> = tys
            .iter()
            .map(|_| self.f.add_param(o, MType::VAL_TOP))
            .collect();
        self.f
            .frame
            .depths
            .insert(pc, u32::try_from(depth).unwrap());
        let fail = self.new_block();
        self.at(fail);
        let (nargs, nlocals) = (self.s.nargs, self.s.nlocals);
        self.term(Opcode::Exit { pc, nargs, nlocals }, params.clone(), vec![]);
        self.at(o);
        let mut vals = self.guard_to(&params, &tys, fail).into_iter();
        self.st = tys
            .iter()
            .map(|&ty| match ty {
                Ty::Dead => Slot::dead(),
                ty => Slot {
                    v: vals.next().unwrap(),
                    ty,
                },
            })
            .collect();
        self.live = true;
        Ok(())
    }

    /// Enter leader `pc`: its entry types are the table's (what back edges
    /// brought in earlier runs) joined with every forward edge of this run.
    /// Fills the pending trampolines. `None` if nothing reaches it.
    pub(super) fn enter(&mut self, pc: Pc) -> Option<Vec<Ty>> {
        let pending = self.pending.remove(&pc).unwrap_or_default();
        let mut tys = self.table.get(&pc).cloned();
        for (_, t, _) in &pending {
            tys = Some(match tys {
                None => t.clone(),
                Some(mut a) => {
                    for (x, y) in a.iter_mut().zip(t) {
                        *x = x.join(*y);
                    }
                    a
                }
            });
        }
        let mut tys = tys?;
        self.s.depths.at(pc)?;
        // Slots guarded on the way into the loop (§10.2): native objects in
        // the loop, however they came in.
        for (i, t) in tys.iter_mut().enumerate() {
            if let Some(&n) = self.hoist.get(&(pc, i)) {
                if matches!(*t, Ty::Val(u) if TagSet::OBJECT.subset_of(u)) {
                    *t = n;
                }
            }
        }
        // Slots dead here take no param.
        let live = &self.s.live[&pc];
        for (t, &l) in tys.iter_mut().zip(live) {
            if !l {
                *t = Ty::Dead;
            }
        }
        self.entry_types.insert(pc, tys.clone());
        let saved = (self.cur, self.live);
        for (t, ttys, from) in pending {
            let target = self.block_for(pc, from).unwrap();
            self.at(t);
            let params = self.f.blocks[t].params.clone();
            let mut params = params.into_iter();
            // The edge's frame, for a hoisted guard's exit (at `pc`,
            // before the loop runs).
            let frame: Vec<Slot> = {
                let mut ps = self.f.blocks[t].params.clone().into_iter();
                ttys.iter()
                    .map(|&ty| match ty {
                        Ty::Dead => Slot::dead(),
                        ty => Slot {
                            v: ps.next().unwrap(),
                            ty,
                        },
                    })
                    .collect()
            };
            let mut exit_b = None;
            let mut args = vec![];
            for (&from_ty, &to_ty) in ttys.iter().zip(&tys) {
                let v = if from_ty == Ty::Dead {
                    None
                } else {
                    params.next()
                };
                if to_ty == Ty::Dead {
                    continue;
                }
                let v = v.expect("a slot live at a block is live on each edge into it");
                if matches!((from_ty, to_ty), (Ty::Val(_), Ty::Native | Ty::Ta(_))) {
                    let fail = *exit_b.get_or_insert_with(|| {
                        let here = self.cur;
                        let b = self.new_block();
                        self.at(b);
                        let ops = self.exit_operands(pc, &frame);
                        let eop = self.exit_op(pc, false);
                        self.term(eop, ops, vec![]);
                        self.at(here);
                        b
                    });
                    let o = self.guard_narrow(v, to_ty, fail);
                    args.push(EdgeArg::Value(o));
                    continue;
                }
                args.push(EdgeArg::Value(self.convert(Slot { v, ty: from_ty }, to_ty)));
            }
            self.term(
                Opcode::Jump,
                vec![],
                vec![Edge {
                    block: target,
                    args,
                }],
            );
        }
        (self.cur, self.live) = saved;
        Some(tys)
    }

    /// The entry root: callee, `this`, formals. Formals with a claim are
    /// guarded at their definition; locals and rval start undefined.
    pub(super) fn entry(&mut self) {
        let b0 = self.cur;
        self.f.roots.push(Root {
            kind: RootKind::Entry,
            block: b0,
        });
        let callee = MType::Obj(ObjInfo::kind(ObjKind::Function(Some(self.s.sid))));
        self.f.add_param(b0, callee);
        let all = Ty::Val(TagSet::ALL);
        // An object under construction the inlining `new` or call passes.
        let this_ty = match (self.s.this_ctor, self.s.this_obj) {
            (Some((k, n, t)), _) => Ty::Ctor(k, n, t),
            // A published object of a proven type the inlining call passes.
            (None, Some((lo, hi, t, sl))) => Ty::Obj(
                KeyRange {
                    lo: crate::ids::LayoutKey::new(lo),
                    hi: crate::ids::LayoutKey::new(hi),
                },
                t,
                sl,
            ),
            (None, None) => all,
        };
        let this = self.f.add_param(b0, this_ty.mir());
        self.st.push(Slot {
            v: this,
            ty: this_ty,
        });
        for i in 0..self.s.nargs {
            // A closure the inlining call passes (`formal_fns`).
            let ty = match self.s.formal_fns.get(i as usize) {
                Some(&Some(k)) => Ty::Fn(k),
                _ => all,
            };
            let v = self.f.add_param(b0, ty.mir());
            self.st.push(Slot { v, ty });
            // The formal's advisory class (unguarded
            // until a typed access checks it).
            let key = (self.s.sid, ArgIndex::new(i + 1));
            if let Some(&(lo, hi)) = self.s.ctx.facts.arg_cls.get(&key) {
                if lo == hi && !self.s.script.has_mapped_args {
                    self.slot_cls.insert(self.st.len() - 1, lo.get());
                }
            }
        }
        let undef = self.const_val(ConstVal::Undefined);
        let uty = Ty::Val(TagSet::prims(PRIM_UNDEFINED));
        for _ in 0..=self.s.nlocals {
            self.st.push(Slot { v: undef, ty: uty });
        }
        self.pc = Pc::new(0);
        self.pre = self.st.clone();
        if self.mapped() {
            // First, before anything can exit: baseline, resumed, reads
            // the formals through it.
            self.js_static(Opcode::ArgsObject, vec![], MType::val(TagSet::OBJECT));
        }
        for i in 0..self.s.nargs {
            if self.mapped() {
                break;
            }
            let (op, ty) = match self.s.arg_claim(i) {
                Ty::I32 => (Opcode::GuardUnbox(UnboxKind::I32), Ty::I32),
                Ty::F64 => (Opcode::GuardUnbox(UnboxKind::F64Num), Ty::F64),
                _ => continue,
            };
            let ix = self.arg_ix(i);
            let v = self.guard(op, vec![self.st[ix].v], ty.mir());
            self.st[ix] = Slot { v, ty };
        }
        self.jump_to(Pc::new(0), None);
    }
}
