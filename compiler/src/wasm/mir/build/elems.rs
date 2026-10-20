//! elems: methods of `Run`, split out of `build.rs` unchanged.

use super::*;

impl<'s, 'a> Run<'s, 'a> {
    /// An element access at `pc` the analysis predicts on a typed array of
    /// kind k (`ta_elem_sites`), with an int32 key: the receiver as that
    /// typed array (proven by its slot, or guarded here, exiting on a
    /// miss, every slot holding it refined), the raw index and the kind.
    /// Not a Uint32Array (its elements are not int32s).
    pub(super) fn ta_elem(
        &mut self,
        pc: Pc,
        recv: Slot,
        key: Slot,
    ) -> Option<(mir::Value, mir::Value, crate::opsem::TaKind)> {
        let k = match recv.ty {
            Ty::Ta(k) => k,
            _ => *self.s.ctx.facts.ta_elem_sites.get(&self.site(pc))?,
        };
        if k == crate::opsem::TaKind::Uint32 || key.ty.num() != Some(Num::I32) {
            return None;
        }
        let o = match recv.ty {
            Ty::Ta(_) => recv.v,
            Ty::Obj(..) | Ty::ObjHint(_) | Ty::Native => self.guard(
                Opcode::GuardKind(ObjKind::TypedArray(k)),
                vec![recv.v],
                Ty::Ta(k).mir(),
            ),
            Ty::Val(t) if TagSet::OBJECT.subset_of(t) => {
                let u = self.guard(
                    Opcode::GuardUnbox(UnboxKind::Obj),
                    vec![recv.v],
                    MType::OBJ_TOP,
                );
                let o = self.guard(
                    Opcode::GuardKind(ObjKind::TypedArray(k)),
                    vec![u],
                    Ty::Ta(k).mir(),
                );
                for x in self.st.iter_mut() {
                    if x.v == recv.v {
                        *x = Slot {
                            v: o,
                            ty: Ty::Ta(k),
                        };
                    }
                }
                o
            }
            _ => return None,
        };
        let i = self.as_i32(key);
        Some((o, i, k))
    }

    /// An element access at `pc` the analysis predicts on an array with an
    /// int32 key: the receiver as a native object (proven by its slot, or
    /// guarded here, exiting on a miss, and every slot holding it then
    /// refined) and the raw index. `None` for anything else.
    pub(super) fn native_elem(
        &mut self,
        pc: Pc,
        recv: Slot,
        key: Slot,
    ) -> Option<(mir::Value, mir::Value)> {
        // The analysis saw the site read numbers or objects (not a
        // string's characters), or write, or knows the array's class.
        let site = self.site(pc);
        let facts = &self.s.ctx.facts;
        let predicted = self.s.ctx.array_elem_in.contains_key(&site)
            || facts.elem_write_sites.contains_key(&site)
            || facts.elem_sites.get(&site).is_some_and(|c| {
                c.is_object() || (!c.prims().is_empty() && c.prims().subset_of(crate::opsem::NUM))
            });
        if (recv.ty != Ty::Native && !predicted)
            || key.ty.num() != Some(Num::I32)
            || !TagSet::OBJECT.subset_of(recv.ty.tags())
        {
            return None;
        }
        let i = self.as_i32(key);
        let o = match recv.ty {
            Ty::Native => recv.v,
            Ty::Obj(..) | Ty::ObjHint(_) => self.guard(
                Opcode::GuardKind(ObjKind::Native),
                vec![recv.v],
                Ty::Native.mir(),
            ),
            Ty::Val(_) => {
                let u = self.guard(
                    Opcode::GuardUnbox(UnboxKind::Obj),
                    vec![recv.v],
                    MType::OBJ_TOP,
                );
                let o = self.guard(
                    Opcode::GuardKind(ObjKind::Native),
                    vec![u],
                    Ty::Native.mir(),
                );
                for x in self.st.iter_mut() {
                    if x.v == recv.v {
                        *x = Slot {
                            v: o,
                            ty: Ty::Native,
                        };
                    }
                }
                o
            }
            _ => return None,
        };
        Some((o, i))
    }

    /// Receiver `recv` of a typed site, when its slot type proves the
    /// site's layouts: the object (for an `ObjHint`, guarded again,
    /// exiting here on a miss, and every slot holding it refined), and
    /// the site as it applies to it: with TYPES only if the slot has the
    /// bit proven (a site that would guard for the bit reads under
    /// identity alone, and its claim is guarded at the def).
    pub(super) fn proven_recv(
        &mut self,
        recv: Slot,
        site: &TypedSite,
    ) -> Option<(mir::Value, TypedSite)> {
        let (keys, types, slots) = match recv.ty {
            Ty::Obj(keys, types, slots) => (keys, Some(types), slots),
            Ty::ObjHint(keys) => (keys, None, false),
            _ => return None,
        };
        let want = KeyRange {
            lo: crate::ids::LayoutKey::new(site.lo),
            hi: crate::ids::LayoutKey::new(site.hi),
        };
        if !want.contains(&keys) {
            return None;
        }
        let site = TypedSite {
            types: site.types && types == Some(true),
            ..*site
        };
        // A field with a slot prediction needs SLOTS proven: a receiver
        // proven without it (or only hinted) is guarded (again) with it.
        let o = match types {
            Some(_) if slots || !site.slotted => recv.v,
            _ => {
                let t = types.unwrap_or(false);
                let s = site.slotted;
                let ty = Ty::Obj(keys, t, s);
                let g = self.guard(
                    Opcode::GuardLayout {
                        keys,
                        types: t,
                        slots: s,
                        closed: false,
                    },
                    vec![recv.v],
                    ty.mir(),
                );
                for x in self.st.iter_mut() {
                    if x.v == recv.v {
                        *x = Slot { v: g, ty };
                    }
                }
                g
            }
        };
        Some((o, site))
    }

    /// `x[k] = y` (`post` the value, left on the stack): with an int32 key,
    /// a set that runs no code (`setelem.data`) where a demotion can exit
    /// after it, a set that would run code exiting here, before the op;
    /// else the generic set.
    pub(super) fn set_elem(
        &mut self,
        strict: bool,
        duty: bool,
        x: mir::Value,
        k: mir::Value,
        y: mir::Value,
        post: Slot,
    ) {
        let int_key =
            matches!(self.f.ty(k), MType::Val(v) if v.tags.is_nonempty_subset_of(TagSet::INT32));
        if int_key {
            if let Some(dirty) = self.dirty_exit(None, Some(post)) {
                let ok = self.new_block();
                let fail = self.exit_block(false);
                let err = self.exit_block(true);
                let op = Opcode::SetElemData(duty);
                self.retain_locals(&op);
                let (inst, _) = self.f.add_inst(
                    self.cur,
                    op,
                    vec![x, k, y],
                    &[],
                    vec![Self::goto(ok), dirty, Self::goto(fail), Self::goto(err)],
                );
                self.attach_targets(inst);
                self.live = false;
                self.at(ok);
                return;
            }
        }
        self.js_void_keep(Opcode::JsSetElem(strict, duty), vec![x, k, y], post);
    }

    /// `x[k]` as a data read (`getelem.data`): a read that would run code
    /// or throw exits here, before the op, for baseline to do it; an
    /// engine error takes the throw exit.
    pub(super) fn get_elem(&mut self, x: mir::Value, k: mir::Value) -> mir::Value {
        let ok = self.new_block();
        let r = self.f.add_param(ok, MType::VAL_TOP);
        let fail = self.exit_block(false);
        let err = self.exit_block(true);
        self.term(
            Opcode::GetElemData,
            vec![x, k],
            vec![
                Edge {
                    block: ok,
                    args: vec![EdgeArg::Out(0)],
                },
                Self::goto(fail),
                Self::goto(err),
            ],
        );
        self.at(ok);
        r
    }

    /// Whether a by-name read of `a` can be `getprop.data`: no getter the
    /// analysis knows of has its name (`getter_names`), so a lookup that
    /// would run code is rare, and exits.
    pub(super) fn data_get_ok(&self, a: mir::entity::AtomId) -> bool {
        self.s.names.is_some_and(|ns| {
            ns.lookup(self.mm.atoms[a].chars())
                .is_none_or(|n| !self.s.ctx.facts.getter_names.contains(&n))
        })
    }

    /// `x.a` as a data read (`getprop.data`, MIR-MEMORY.md §1): ok with
    /// the value; a lookup that would run code or throw exits here, before
    /// the op, for baseline to do it.
    pub(super) fn data_get(&mut self, a: mir::entity::AtomId, x: mir::Value) -> mir::Value {
        let ok = self.new_block();
        let r = self.f.add_param(ok, MType::VAL_TOP);
        let exit = self.exit_block(false);
        self.term(
            Opcode::GetPropData(a),
            vec![x],
            vec![
                Edge {
                    block: ok,
                    args: vec![EdgeArg::Out(0)],
                },
                Self::goto(exit),
            ],
        );
        self.at(ok);
        r
    }

    /// `switch (x)` over string constants, as the bytecode spells each case:
    /// `Dup; String s; StrictEq; Case target` (true pops `x` and jumps,
    /// false keeps it), the next one after the fallthrough's `JumpTarget`. Where `SWITCH_STR_MIN` or more start at the `Dup` at
    /// `self.pc`, one `switch.str` decides them all: each case's edge
    /// without `x`, the default edge to the op after the last `Case` with
    /// it, and the fail edge an exit here, before the chain. The chain's
    /// other ops are then reached by no edge, and the walk skips them.
    /// Returns whether it took the `Dup`.
    pub(super) fn string_switch(&mut self) -> R<bool> {
        let pc = self.pc;
        let mut cases: Vec<(u32, Pc)> = vec![];
        // `at`: the op after the last case's `Case` (the default's start);
        // the next case's `Dup` follows that fallthrough's `JumpTarget`.
        let mut at = pc;
        loop {
            let mut d = at;
            while !cases.is_empty() && self.s.op_at(d) == Some(JSOp::JumpTarget) {
                d += JSOp::JumpTarget.len();
            }
            let s_pc = d + JSOp::Dup.len();
            let e_pc = s_pc + JSOp::String.len();
            let c_pc = e_pc + JSOp::StrictEq.len();
            if self.s.op_at(d) != Some(JSOp::Dup)
                || self.s.op_at(s_pc) != Some(JSOp::String)
                || self.s.op_at(e_pc) != Some(JSOp::StrictEq)
                || self.s.op_at(c_pc) != Some(JSOp::Case)
            {
                break;
            }
            let idx = self.s.imms(s_pc).next_uint32().unwrap();
            let off = self.s.imms(c_pc).next_int32().unwrap();
            cases.push((idx, c_pc.branch(off)));
            at = c_pc + JSOp::Case.len();
        }
        if cases.len() < SWITCH_STR_MIN {
            return Ok(false);
        }
        let fail = self.exit_block(false);
        let x = self.top();
        let mut args = vec![self.boxed(x)];
        for &(idx, _) in &cases {
            let a = self.atom(idx)?;
            let s = self.inst(
                Opcode::ConstStr(a),
                vec![],
                Some(MType::Str(mir::types::StrInfo { atom: Some(a) })),
            );
            args.push(s);
        }
        let lval = self.pop();
        let mut edges = vec![];
        for &(_, target) in &cases {
            edges.push(self.edge_to(target, Some(pc)));
        }
        self.st.push(lval);
        edges.push(self.edge_to(at, Some(pc)));
        match edges.into_iter().collect::<Option<Vec<Edge>>>() {
            Some(mut edges) => {
                edges.push(Self::goto(fail));
                let n = u32::try_from(cases.len()).unwrap();
                self.term(Opcode::SwitchStr(n), args, edges);
            }
            None => self.term(Opcode::Unreachable, vec![], vec![]),
        }
        Ok(true)
    }
}
