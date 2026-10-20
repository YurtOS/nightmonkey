//! ctors: methods of `Run`, split out of `build.rs` unchanged.

use super::*;

impl<'s, 'a> Run<'s, 'a> {
    /// At a layout constructor's return, the first stamp of its completed
    /// `this` (a no-op unless `this` is still under construction, so a
    /// call without `new` stamps nothing); an inlined copy stamps too.
    pub(super) fn ctor_stamp(&mut self) {
        let Some(si) = self.s.ctx.stamp_ctors_in.get(&self.s.sid) else {
            return;
        };
        // Published on this path (`publish_layout`): nothing to stamp.
        if matches!(self.st[0].ty, Ty::Obj(r, _, true) if r == KeyRange::one(crate::ids::LayoutKey::new(si.layout_id)))
        {
            return;
        }
        let op = Opcode::CtorStamp(
            si.layout_id,
            u32::try_from(si.fields.len()).unwrap(),
            crate::wasm::mir::stamp::typed_keep_bits(self.s.ctx, si),
        );
        let t = self.boxed(self.st[0]);
        self.inst(op, vec![t], None);
    }

    /// `publish_layout` at its earliest point (MIR.md §2.3): before a call
    /// made while `this` may be under construction (`ctor_publish`), the
    /// stamp of each constructor whose early key it carries, once all of
    /// that constructor's fields are there. A method it calls on itself
    /// then finds a published object (its entry guard passes, and its
    /// stores keep TYPES), where it would otherwise exit to baseline and
    /// leave the object published without TYPES for good.
    pub(super) fn publish_this(&mut self) {
        let ctx = self.s.ctx;
        let Some(ctors) = ctx.facts.ctor_publish.get(&self.s.sid) else {
            return;
        };
        let Some(&x) = self.st.first() else { return };
        if matches!(
            x.ty,
            Ty::Dead | Ty::Obj(..) | Ty::ObjHint(..) | Ty::Ctor(..)
        ) {
            return;
        }
        let t = self.boxed(x);
        for c in ctors {
            let Some(si) = ctx.stamp_ctors_in.get(c) else {
                continue;
            };
            let op = Opcode::CtorPublish(
                si.layout_id,
                u32::try_from(si.fields.len()).unwrap(),
                crate::wasm::mir::stamp::typed_keep_bits(self.s.ctx, si),
            );
            self.inst(op, vec![t], None);
        }
    }

    /// At an init delegate's or a fill script's return, the two-phase
    /// restamp of `this` or the named formal (`deleg_restamps_in`,
    /// `arg_restamps_in`); an inlined copy's returns restamp its own.
    pub(super) fn return_restamps(&mut self) {
        let ctx = self.s.ctx;
        let sid = self.s.sid;
        if let Some(si) = ctx.deleg_restamps_in.get(&sid) {
            if self.st[0].ty != Ty::Dead {
                let t = self.boxed(self.st[0]);
                self.restamp(si, t);
            }
        }
        if let Some((formal, si)) = ctx.arg_restamps_in.get(&sid) {
            if let Some(v) = self.formal_now(*formal) {
                self.restamp(si, v);
            }
        }
    }

    /// After the last add of a post-construction fill sequence
    /// (`local_restamps_in`), the restamp of the named local or formal.
    pub(super) fn local_restamp(&mut self, pc: Pc) {
        let Some((local, si)) = self.s.ctx.local_restamps_in.get(&self.site(pc)) else {
            return;
        };
        let v = if local & crate::facts::RESTAMP_FORMAL != 0 {
            self.formal_now(local & !crate::facts::RESTAMP_FORMAL)
        } else {
            let x = self.st[self.local_ix(*local)];
            (x.ty != Ty::Dead).then(|| self.boxed(x))
        };
        if let Some(v) = v {
            self.restamp(si, v);
        }
    }

    /// Formal `n`'s current value, boxed; `None` if dead here (nothing
    /// reads it, so nothing reads the object through it).
    pub(super) fn formal_now(&mut self, n: u32) -> Option<mir::Value> {
        if n >= self.s.nargs {
            return None;
        }
        if self.mapped() {
            return Some(self.inst(Opcode::ArgsMapped(n), vec![], Some(MType::VAL_TOP)));
        }
        let x = self.st[self.arg_ix(n)];
        (x.ty != Ty::Dead).then(|| self.boxed(x))
    }

    pub(super) fn restamp(&mut self, si: &StampCtorIn, v: mir::Value) {
        let Some(r) = crate::wasm::mir::stamp::typed_restamp_args(self.s.ctx, si) else {
            return;
        };
        let i = u32::try_from(self.mm.restamps.len()).unwrap();
        self.mm.restamps.push(r);
        self.inst(Opcode::Restamp(i), vec![v], None);
    }

    /// A fallible check: continue on success with its output (of type
    /// `out`), exit at the op's pc on failure.
    /// Layout `key`'s fields in slot order, each with its claim (MIR.md
    /// §2.3: the row a constructor builds, `stamp_ctors_in`), registered
    /// in the module's layout table for `init_field` and `publish_layout`.
    /// `None` if no constructor builds it, or the table already describes
    /// one of its slots otherwise.
    pub(super) fn ctor_layout(
        &mut self,
        key: u32,
    ) -> Option<std::rc::Rc<Vec<(mir::entity::AtomId, MType)>>> {
        if let Some(l) = self.ctor_layouts.get(&key) {
            return l.clone();
        }
        let l = self.make_ctor_layout(key).map(std::rc::Rc::new);
        self.ctor_layouts.insert(key, l.clone());
        l
    }

    pub(super) fn make_ctor_layout(
        &mut self,
        key: u32,
    ) -> Option<Vec<(mir::entity::AtomId, MType)>> {
        let ctx = self.s.ctx;
        // A constructor's row, or, for a construct through a shared
        // wrapper (`Class.create`'s), the site's or its init delegate's.
        let si = ctx
            .stamp_ctors_in
            .values()
            .chain(ctx.construct_sites_in.values())
            .chain(ctx.deleg_restamps_in.values())
            .find(|si| si.layout_id == key)?;
        let names = self.s.names?;
        let claims = ctx
            .layout_field_types_in
            .get(&crate::ids::LayoutKey::new(key).stamp());
        let out: Vec<(mir::entity::AtomId, MType)> = si
            .fields
            .iter()
            .map(|&n| {
                let a = self.mm.intern_atom(names.get(n).chars());
                let claim = match claims.and_then(|m| m.get(&n)).filter(|c| !c.is_none()) {
                    Some(&c) => MType::val(claim_tags(c)),
                    None => MType::VAL_TOP,
                };
                (a, claim)
            })
            .collect();
        let closed = self.s.ctx.closed_layouts_in.contains(&key);
        let l = self
            .mm
            .layouts
            .entry(crate::ids::LayoutKey::new(key))
            .or_default();
        l.closed |= closed;
        if l.fields.len() > out.len() {
            return None;
        }
        l.fields.resize(out.len(), None);
        for (i, &(a, claim)) in out.iter().enumerate() {
            match &l.fields[i] {
                None => l.fields[i] = Some(mir::module::FieldDef { name: a, claim }),
                Some(f) if f.name == a && f.claim == claim => {}
                Some(_) => return None,
            }
        }
        Some(out)
    }

    /// The layout a constructor `k`'s `this` is under construction for,
    /// and its TYPES, when a construct allocates it with `word`: the
    /// sentinel with `k`'s own early key.
    pub(super) fn ctor_of_word(&mut self, k: ScriptId, word: u32) -> Option<(u32, bool)> {
        if word & crate::wasm::mir::abi::CLASS_WORD_SENTINEL == 0 {
            return None;
        }
        let early = (word >> crate::wasm::mir::abi::EARLY_KEY_SHIFT) & 0xFFF;
        // A shared wrapper constructor has no row of its own: the site's
        // (`construct_sites_in`) put its key in the word.
        let key = match self.s.ctx.stamp_ctors_in.get(&k) {
            Some(si) => si.layout_id,
            None => early.checked_sub(1)?,
        };
        if early != key + 1 {
            return None;
        }
        self.ctor_layout(key)?;
        Some((key, word & crate::wasm::mir::abi::CLASS_WORD_SHALLOW != 0))
    }

    /// `recv.f` for an object under construction (§2.3) whose field `f`
    /// is already added: the field, typed by its claim where TYPES holds.
    /// `false` (nothing built) for any other access.
    pub(super) fn ctor_get(&mut self, next: Pc, a: mir::entity::AtomId, recv: Slot) -> bool {
        let Ty::Ctor(key, n, t) = recv.ty else {
            return false;
        };
        let Some(fields) = self.ctor_layout(key) else {
            return false;
        };
        let Some(i) = fields.iter().position(|&(f, _)| f == a) else {
            return false;
        };
        if !n.contains(u32::try_from(i).unwrap()) {
            return false;
        }
        let claim = if t { fields[i].1 } else { MType::VAL_TOP };
        let r = self
            .js_dirty_exits(Opcode::LoadField(a), vec![recv.v], Some(claim), next, None)
            .unwrap();
        let tags = match claim {
            MType::Val(s) => s.tags,
            _ => TagSet::ALL,
        };
        let r = self.weaken(r, MType::val(tags));
        self.push(r, Ty::Val(tags));
        true
    }

    /// `recv.f = v` for an object under construction (§2.3): the add of
    /// the next field its layout predicts is an `init_field` (its value
    /// guarded to the field's claim, exiting here on a miss), advancing
    /// `recv` and every copy of it, and publishing the layout
    /// (`publish_layout`) once every field is there; a store to a field
    /// already added is a typed `store_field`. A boxed object a fence
    /// demoted from under construction (`ctor_hint`) is guarded back to
    /// the state the add expects (`guard.ctor`) first. `false` (nothing
    /// built) for any other store.
    pub(super) fn ctor_set(
        &mut self,
        next: Pc,
        a: mir::entity::AtomId,
        recv: Slot,
        v: Slot,
    ) -> bool {
        let (key, t, hint) = match recv.ty {
            Ty::Ctor(key, _, t) => (key, t, None),
            Ty::Val(_) => match self.ctor_hint.get(&recv.v) {
                Some(&(key, n, t)) => (key, t, Some(n)),
                None => return false,
            },
            _ => return false,
        };
        let Some(fields) = self.ctor_layout(key) else {
            return false;
        };
        let Some(i) = fields.iter().position(|&(f, _)| f == a) else {
            return false;
        };
        let iu = u32::try_from(i).unwrap();
        let n = match recv.ty {
            Ty::Ctor(_, n, _) => n,
            _ => hint.unwrap(),
        };
        if !n.contains(iu) && !n.may_add(iu) {
            return false;
        }
        let recv = if let Some(n) = hint {
            // The state the object was left in.
            let o = self.guard(
                Opcode::GuardUnbox(UnboxKind::Obj),
                vec![recv.v],
                MType::OBJ_TOP,
            );
            let g = Opcode::GuardCtor {
                key: crate::ids::LayoutKey::new(key),
                n,
                types: t,
            };
            let ty = Ty::Ctor(key, n, t);
            let c = self.guard(g, vec![o], ty.mir());
            let s = Slot { v: c, ty };
            self.replace_slot(recv.v, s);
            s
        } else {
            recv
        };
        let all = FieldSet::prefix(u32::try_from(fields.len()).unwrap());
        // A leading store `new_this.init` already made (the value is the
        // formal's, which nothing before it can have changed).
        let pre = self.s.prefill_pcs.len();
        if self.s.prefill_pcs.get(i) == Some(&self.pc)
            && n == FieldSet::prefix(u32::try_from(pre).unwrap())
        {
            if i + 1 == pre && n == all {
                self.publish_ctor(key, t, recv.v);
            }
            return true;
        }
        // The value, of the field's claim: statically, or guarded here.
        let claim = fields[i].1;
        let ctags = match claim {
            MType::Val(s) => s.tags,
            _ => TagSet::ALL,
        };
        let x = self.boxed(v);
        let x = if v.ty.tags().is_nonempty_subset_of(ctags) {
            self.weaken(x, claim)
        } else {
            self.guard(Opcode::GuardTags(ctags), vec![x], claim)
        };
        if n.contains(iu) {
            let x = if t { x } else { self.weaken(x, MType::VAL_TOP) };
            self.store_mask = Some((vec![(key + 1, ctags)], false));
            self.js_dirty_exits(Opcode::StoreField(a), vec![recv.v, x], None, next, Some(v));
            return true;
        }
        let ty = Ty::Ctor(key, n.with(iu), t);
        let o = self.guard(Opcode::InitField(a), vec![recv.v, x], ty.mir());
        let s = Slot { v: o, ty };
        self.replace_slot(recv.v, s);
        if n.with(iu) == all {
            self.publish_ctor(key, t, o);
        }
        true
    }

    /// Every field of layout `key` is on object `o`: the stamp. It ends
    /// construction, so it kills the claims on the object under
    /// construction, as its prediction witness says (§4.5); every copy of
    /// it is replaced by the published one.
    pub(super) fn publish_ctor(&mut self, key: u32, t: bool, o: mir::Value) {
        let pty = Ty::Obj(KeyRange::one(crate::ids::LayoutKey::new(key)), t, true);
        let (inst, rs) = self.f.add_inst(
            self.cur,
            Opcode::PublishLayout,
            vec![o],
            &[pty.mir()],
            vec![],
        );
        self.f.witnesses[inst] = Some(mir::func::Witness {
            may_kill: mir::types::KillPattern::of(mir::types::KillSet::CONSTRUCTING),
        });
        self.replace_slot(o, Slot { v: rs[0], ty: pty });
        // A builder run's only constructing object is its own `this`, so
        // the frame's `this` is now this published object (a standalone
        // constructor's raw `this` slot never saw the guard that refined
        // its boxed copy): `ctor_stamp` then has nothing left to do.
        if self
            .s
            .ctx
            .stamp_ctors_in
            .get(&self.s.sid)
            .is_some_and(|si| si.layout_id == key)
        {
            self.st[0] = Slot { v: rs[0], ty: pty };
        }
    }

    /// An object under construction `x` passed as a callee's `this`
    /// (`inline_call_this`): the callee, built for it, gets `x` itself;
    /// this frame continues with `boxed` (its boxed copy), hinted
    /// (`ctor_hint`), since the callee may add fields or publish it.
    pub(super) fn ctor_pass(&mut self, x: Slot, boxed: mir::Value) -> mir::Value {
        let Ty::Ctor(k, n, t) = x.ty else {
            unreachable!("ctor_pass of {:?}", x.ty)
        };
        self.ctor_hint.insert(boxed, (k, n, t));
        let y = Slot {
            v: boxed,
            ty: Ty::Val(TagSet::OBJECT),
        };
        for s in self
            .st
            .iter_mut()
            .chain(self.pre.iter_mut())
            .filter(|s| s.v == x.v)
        {
            *s = y;
        }
        self.renames.push((x.v, y));
        x.v
    }

    /// At a return, the construction state `this` is left in
    /// (`this_out`).
    pub(super) fn note_this_out(&mut self) {
        let now = match self.st.first().map(|x| x.ty) {
            Some(Ty::Ctor(k, n, t)) => Some((k, n, t)),
            _ => None,
        };
        self.this_out = Some(match self.this_out {
            None => now,
            Some(prev) if prev == now => now,
            Some(_) => None,
        });
    }

    /// The state of an object under construction `boxed`, passed to
    /// inlined `targets` as `this`, after them: every callee's `this_out`,
    /// where they agree, is its next use's guard (`ctor_hint`).
    pub(super) fn ctor_after(
        &mut self,
        targets: &[(ScriptId, std::rc::Rc<super::super::inline::Callee>)],
        boxed: mir::Value,
    ) {
        let outs: Vec<Option<(u32, FieldSet, bool)>> =
            targets.iter().map(|(_, c)| c.this_out).collect();
        match outs.first() {
            Some(&Some(o)) if outs.iter().all(|&x| x == Some(o)) => {
                self.ctor_hint.insert(boxed, o);
            }
            _ => {
                self.ctor_hint.remove(&boxed);
            }
        }
    }

    /// A boxed receiver `x` a fence demoted from under construction
    /// (`ctor_hint`), guarded back to the state it was left in, for a call
    /// whose callees are built for it; exiting here on a miss.
    pub(super) fn ctor_reguard(&mut self, x: Slot) -> Option<Slot> {
        let &(k, n, t) = self.ctor_hint.get(&x.v)?;
        if !matches!(x.ty, Ty::Val(_)) {
            return None;
        }
        let o = self.guard(
            Opcode::GuardUnbox(UnboxKind::Obj),
            vec![x.v],
            MType::OBJ_TOP,
        );
        let g = Opcode::GuardCtor {
            key: crate::ids::LayoutKey::new(k),
            n,
            types: t,
        };
        let ty = Ty::Ctor(k, n, t);
        let c = self.guard(g, vec![o], ty.mir());
        let s = Slot { v: c, ty };
        self.replace_slot(x.v, s);
        Some(s)
    }

    /// For an inlined `new` of constructor `k` (allocation word `word`)
    /// with arguments `args`: its leading formal stores
    /// (`ctor_formal_prefix`) that are its layout's first fields, in order,
    /// as the layout key, TYPES, the values (of the fields' claims: guarded
    /// here, exiting before the `new`) and the body built for the object
    /// holding them. `None` where there are none.
    #[allow(clippy::type_complexity)]
    pub(super) fn ctor_prefill(
        &mut self,
        k: ScriptId,
        word: u32,
        args: &[mir::Value],
    ) -> Option<(
        u32,
        bool,
        Vec<mir::Value>,
        std::rc::Rc<super::super::inline::Callee>,
    )> {
        let (key, t) = self.ctor_of_word(k, word)?;
        let ctx = self.s.ctx;
        let crate::source::SourceObject::Script(ks) = ctx
            .source
            .object(crate::source::SourceObjectId::new(k.get()))
        else {
            return None;
        };
        let fields = self.ctor_layout(key)?;
        let mut formals: Vec<usize> = vec![];
        for (gc, j, _) in ctor_formal_prefix(ks) {
            let i = formals.len();
            if i >= fields.len() || usize::from(j) >= args.len() {
                break;
            }
            let named = ks.gcthings.get(gc as usize).is_some_and(|&g| {
                !g.is_other()
                    && matches!(ctx.source.object(g), crate::source::SourceObject::String(s) if s.chars() == self.mm.atoms[fields[i].0].chars())
            });
            if !named {
                break;
            }
            formals.push(usize::from(j));
        }
        if formals.is_empty() {
            return None;
        }
        let n = u32::try_from(formals.len()).unwrap();
        let c = self.s.callee_ctx(
            k,
            &[],
            Some((key, FieldSet::prefix(n), t)),
            None,
            self.admitted,
        )?;
        let xs = formals
            .iter()
            .enumerate()
            .map(|(i, &j)| {
                let (x, claim) = (args[j], fields[i].1);
                let ctags = match claim {
                    MType::Val(s) => s.tags,
                    _ => TagSet::ALL,
                };
                let xtags = match self.f.ty(x) {
                    MType::Val(s) => s.tags,
                    _ => TagSet::ALL,
                };
                if xtags.is_nonempty_subset_of(ctags) {
                    self.weaken(x, claim)
                } else {
                    self.guard(Opcode::GuardTags(ctags), vec![x], claim)
                }
            })
            .collect();
        Some((key, t, xs, c))
    }
}
