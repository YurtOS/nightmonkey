//! flow: methods of `Run`, split out of `build.rs` unchanged.

use super::*;

impl<'s, 'a> Run<'s, 'a> {
    /// The MIR block for leader `pc`, entered from `from` (a pc, or `None`
    /// for the entry root): a loop's preheader when the edge comes from
    /// outside the loop.
    pub(super) fn block_for(&mut self, pc: Pc, from: Option<Pc>) -> Option<mir::Block> {
        let tys = self.entry_types.get(&pc)?.clone();
        let make = |r: &mut Self| {
            let b = r.new_block();
            for t in tys.iter().filter(|&&t| t != Ty::Dead) {
                r.f.add_param(b, t.mir());
            }
            b
        };
        let h = match self.blocks.get(&pc) {
            Some(&b) => b,
            None => {
                let b = make(self);
                self.blocks.insert(pc, b);
                b
            }
        };
        let Some(&end) = self.s.loops.get(&pc) else {
            return Some(h);
        };
        if from.is_some_and(|f| f >= pc && f < end) {
            return Some(h);
        }
        if let Some(&p) = self.preheaders.get(&pc) {
            return Some(p);
        }
        let p = make(self);
        self.preheaders.insert(pc, p);
        let depth = u32::try_from(tys.len() - self.frame_len()).unwrap();
        self.f.frame.depths.entry(pc).or_insert(depth);
        self.f.loops.push(LoopDecl {
            header: h,
            preheader: p,
            entry: Some(mir::func::LoopEntry {
                pc,
                slots: tys.iter().map(|&t| t != Ty::Dead).collect(),
                state: vec![],
            }),
        });
        let saved = (self.cur, self.live);
        self.at(p);
        let args = self.f.blocks[p]
            .params
            .iter()
            .map(|&v| EdgeArg::Value(v))
            .collect();
        self.term(Opcode::Jump, vec![], vec![Edge { block: h, args }]);
        (self.cur, self.live) = saved;
        Some(p)
    }

    /// The edge from the current point (the op at `from`) to leader `to`.
    /// - `to` not entered yet (a forward edge): a trampoline that `to`'s
    ///   entry fills in.
    /// - `to` entered (a loop's back edge): convert the frame to its entry
    ///   types. `None` when they are narrower than the frame's: the run is
    ///   then not the last (`widen`), and the edge is left out.
    pub(super) fn edge_to(&mut self, to: Pc, from: Option<Pc>) -> Option<Edge> {
        // A formal a mapped `arguments` aliases is written through the
        // frame (`ArgsMappedSet`), which leaves its slot dead on this path;
        // where the target takes a value for it, the current one, read
        // back (else the target's entry types never settle).
        if self.mapped() {
            let live = self.s.live.get(&to).cloned();
            for n in 0..self.s.nargs {
                let ix = self.arg_ix(n);
                // Live at the target, whether it is a back edge to a block
                // already entered or a forward edge to one not yet entered
                // (a join, whose entry takes a value for it on every edge).
                if self.st.get(ix).is_some_and(|x| x.ty == Ty::Dead)
                    && live.as_ref().is_some_and(|l| ix < l.len() && l[ix])
                {
                    let v = self.inst(Opcode::ArgsMapped(n), vec![], Some(MType::VAL_TOP));
                    self.st[ix] = Slot {
                        v,
                        ty: Ty::Val(TagSet::ALL),
                    };
                }
            }
        }
        let tys: Vec<Ty> = self.st.iter().map(|x| x.ty).collect();
        match self.out.get_mut(&to) {
            Some(o) => {
                for (a, b) in o.iter_mut().zip(&tys) {
                    *a = a.join(*b);
                }
            }
            None => {
                self.out.insert(to, tys.clone());
            }
        }
        let Some(want) = self.entry_types.get(&to).cloned() else {
            let t = self.new_block();
            let args = self
                .st
                .clone()
                .iter()
                .filter(|x| x.ty != Ty::Dead)
                .map(|x| {
                    self.f.add_param(t, x.ty.mir());
                    EdgeArg::Value(x.v)
                })
                .collect();
            self.pending.entry(to).or_default().push((t, tys, from));
            return Some(Edge { block: t, args });
        };
        if want.len() != tys.len() || !tys.iter().zip(&want).all(|(a, b)| a.fits(*b)) {
            self.widen = true;
            return None;
        }
        if self.s.loops.contains_key(&to) {
            for (i, (&a, &b)) in tys.iter().zip(&want).enumerate() {
                if matches!(a, Ty::Native | Ty::Ta(_))
                    && matches!(b, Ty::Val(t) if TagSet::OBJECT.subset_of(t))
                {
                    self.hoist_req.insert((to, i), a);
                }
            }
        }
        let block = self.block_for(to, from)?;
        let st = self.st.clone();
        let args = st
            .iter()
            .zip(&want)
            .enumerate()
            .filter(|(_, (_, &t))| t != Ty::Dead)
            .map(|(_, (&x, &t))| EdgeArg::Value(self.convert(x, t)))
            .collect();
        Some(Edge { block, args })
    }

    pub(super) fn jump_to(&mut self, to: Pc, from: Option<Pc>) {
        match self.edge_to(to, from) {
            Some(e) => self.term(Opcode::Jump, vec![], vec![e]),
            None => self.term(Opcode::Unreachable, vec![], vec![]),
        }
    }

    /// Branch on raw bool `c` to leaders `then` and `els`.
    pub(super) fn branch(&mut self, c: mir::Value, then: Pc, els: Pc) {
        let from = Some(self.pc);
        match (self.edge_to(then, from), self.edge_to(els, from)) {
            (Some(t), Some(e)) => self.term(Opcode::Br, vec![c], vec![t, e]),
            _ => self.term(Opcode::Unreachable, vec![], vec![]),
        }
    }

    /// ToBoolean of `x` as a raw bool.
    pub(super) fn truthy(&mut self, x: Slot) -> mir::Value {
        match x.ty {
            Ty::Dead => unreachable!("a dead slot is never read"),
            Ty::Bool => x.v,
            // A layout-keyed object, a closure or a typed array never emulates
            // `undefined`; a native object of unproven class may (document.all).
            Ty::Obj(..) | Ty::ObjHint(_) | Ty::Ctor(..) | Ty::Ta(_) | Ty::Fn(_) => {
                self.inst(Opcode::ConstBool(true), vec![], Some(MType::Bool))
            }
            Ty::Native => {
                let v = self.boxed(x);
                self.inst(Opcode::JsToBool, vec![v], Some(MType::Bool))
            }
            Ty::I32 => {
                let z = self.const_i32(0);
                self.inst(
                    Opcode::Cmp(NumRepr::I32, Cc::Ne),
                    vec![x.v, z],
                    Some(MType::Bool),
                )
            }
            // Neither ±0 nor NaN: |x| > 0.
            Ty::F64 => {
                let a = self.inst(Opcode::Math(MathFn::Abs), vec![x.v], Some(MType::F64_TOP));
                let z = self.const_f64(0.0);
                self.inst(
                    Opcode::Cmp(NumRepr::F64, Cc::Gt),
                    vec![a, z],
                    Some(MType::Bool),
                )
            }
            // A boolean (a generic compare's or `instanceof`'s): its bit.
            Ty::Val(t) if t.is_nonempty_subset_of(TagSet::BOOLEAN) => {
                self.inst(Opcode::Unbox(UnboxKind::Bool), vec![x.v], Some(MType::Bool))
            }
            Ty::Val(_) => self.inst(Opcode::JsToBool, vec![x.v], Some(MType::Bool)),
        }
    }

    /// `!c` for a raw bool: a diamond.
    /// `x == null`: null or undefined, or an object that emulates
    /// `undefined`.
    pub(super) fn loose_nullish(&mut self, x: Slot) -> mir::Value {
        let tags = x.ty.tags();
        let nullish = TagSet::prims(PRIM_NULL | PRIM_UNDEFINED);
        let v = self.boxed(x);
        if tags.intersect(TagSet::OBJECT).is_empty() {
            return self.tag_test(v, nullish);
        }
        let (o, prim, j) = (self.new_block(), self.new_block(), self.new_block());
        let r = self.f.add_param(j, MType::Bool);
        let ob = self.f.add_param(o, MType::OBJ_TOP);
        self.term(
            Opcode::GuardUnbox(UnboxKind::Obj),
            vec![v],
            vec![
                Edge {
                    block: o,
                    args: vec![EdgeArg::Out(0)],
                },
                Self::goto(prim),
            ],
        );
        self.at(o);
        let e = self.inst(Opcode::ObjEmulatesUndef, vec![ob], Some(MType::Bool));
        self.term(
            Opcode::Jump,
            vec![],
            vec![Edge {
                block: j,
                args: vec![EdgeArg::Value(e)],
            }],
        );
        self.at(prim);
        let t = self.tag_test(v, nullish);
        self.term(
            Opcode::Jump,
            vec![],
            vec![Edge {
                block: j,
                args: vec![EdgeArg::Value(t)],
            }],
        );
        self.at(j);
        r
    }

    pub(super) fn not(&mut self, c: mir::Value) -> mir::Value {
        let (t, e, j) = (self.new_block(), self.new_block(), self.new_block());
        let p = self.f.add_param(j, MType::Bool);
        self.term(Opcode::Br, vec![c], vec![Self::goto(t), Self::goto(e)]);
        for (b, val) in [(t, false), (e, true)] {
            self.at(b);
            let k = self.inst(Opcode::ConstBool(val), vec![], Some(MType::Bool));
            self.term(
                Opcode::Jump,
                vec![],
                vec![Edge {
                    block: j,
                    args: vec![EdgeArg::Value(k)],
                }],
            );
        }
        self.at(j);
        p
    }
}
