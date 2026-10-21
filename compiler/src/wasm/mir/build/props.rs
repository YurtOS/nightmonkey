//! props: methods of `Run`, split out of `build.rs` unchanged.

use super::*;

impl<'s, 'a> Run<'s, 'a> {
    /// Whether atom `a` is `s`.
    pub(super) fn atom_is(&self, a: mir::entity::AtomId, s: &str) -> bool {
        self.mm.atoms[a]
            .chars()
            .iter()
            .copied()
            .eq(s.encode_utf16())
    }

    /// A fused global's read (§3): while its fuse is armed the read is the
    /// literal, else exit here. False (nothing emitted) for a literal that
    /// is not a primitive constant.
    pub(super) fn fused_gname(
        &mut self,
        name: mir::entity::AtomId,
        fg: crate::wasm::translate::FusedGname,
    ) -> bool {
        use crate::wasm::translate::{TAG_BOOLEAN, TAG_CLEAR, TAG_INT32, TAG_NULL, TAG_UNDEFINED};
        let (tag, payload) = (fg.boxed >> 32, fg.boxed as u32);
        if !(tag == TAG_INT32
            || tag < u64::from(TAG_CLEAR)
            || tag == TAG_BOOLEAN
            || tag == TAG_NULL
            || tag == TAG_UNDEFINED)
        {
            return false;
        }
        let found = self
            .mm
            .fuses
            .iter()
            .find(|(_, d)| d.addr == fg.fuse_addr)
            .map(|(f, _)| f);
        let fid = match found {
            Some(f) => f,
            None => self.mm.fuses.push(mir::module::FuseDef {
                name: self.mm.atoms[name].to_string(),
                addr: fg.fuse_addr,
            }),
        };
        self.guard(
            Opcode::CheckFuse(fid),
            vec![],
            MType::Fact(mir::types::FactKind::Fuse(fid)),
        );
        let (v, ty) = if tag == TAG_INT32 {
            (self.const_i32(payload as i32), Ty::I32)
        } else if tag < u64::from(TAG_CLEAR) {
            (self.const_f64(f64::from_bits(fg.boxed)), Ty::F64)
        } else if tag == TAG_BOOLEAN {
            (
                self.inst(Opcode::ConstBool(payload != 0), vec![], Some(MType::Bool)),
                Ty::Bool,
            )
        } else if tag == TAG_NULL {
            (
                self.const_val(ConstVal::Null),
                Ty::Val(TagSet::prims(PRIM_NULL)),
            )
        } else {
            (
                self.const_val(ConstVal::Undefined),
                Ty::Val(TagSet::prims(PRIM_UNDEFINED)),
            )
        };
        self.push(v, ty);
        true
    }

    /// The environment `hops` links up the chain from the activation's
    /// (which is fixed: see `build`).
    pub(super) fn env_at(&mut self, hops: u16) -> mir::Value {
        let env = MType::Obj(ObjInfo::kind(ObjKind::Env));
        let mut e = self.inst(Opcode::EnvCurrent, vec![], Some(env));
        for _ in 0..hops {
            e = self.inst(Opcode::EnvParent, vec![e], Some(env));
        }
        e
    }

    /// The atom for gcthing `index` (a name), in the module's table.
    pub(super) fn atom(&mut self, index: u32) -> R<mir::entity::AtomId> {
        let gc = *self
            .s
            .script
            .gcthings
            .get(usize::try_from(index).unwrap())
            .ok_or_else(|| format!("name index {index} out of range"))?;
        match self.s.ctx.source.object(gc) {
            crate::source::SourceObject::String(s) => Ok(self.mm.intern_atom(s.chars())),
            _ => Err(format!("name index {index} is not a string")),
        }
    }

    /// The analysis's layout prediction for the property access at `pc`
    /// (`prop_sites`), with its field recorded in the module's layouts;
    /// `None` without one, or when it disagrees with another site's
    /// description of the same slot.
    pub(super) fn typed_site(&mut self, pc: Pc, name: mir::entity::AtomId) -> Option<TypedSite> {
        let ps = *self
            .s
            .ctx
            .prop_sites_in
            .get(&crate::ids::Site::new(self.s.sid, pc))?;
        self.site_for(
            ps.layout_id,
            ps.hi_layout_id,
            ps.slot,
            ps.claim,
            ps.shallow_possible,
            name,
        )
    }

    /// The classes a store of field `name` at `pc` through `recv` checks
    /// its value against, each with its own predicted type
    /// for the field, as an IC validating the object's class before it
    /// stores: every class typing the field (complete) where they are few,
    /// else those the receiver is predicted of (the site row's, the hinted
    /// class, or, through `this`, the script's own row or the layout its
    /// constructor builds).
    pub(super) fn field_claim(
        &mut self,
        pc: Pc,
        name: mir::entity::AtomId,
        recv: Slot,
    ) -> Option<(Vec<(u32, TagSet)>, bool)> {
        // A name the analysis never interned is a field no class types.
        // Spelled out rather than declined: whether a lookup succeeds must
        // not depend on what codegen interned before this build, which
        // reads the analysis-time names (and runs in parallel).
        let all = match self.s.names?.lookup(self.mm.atoms[name].chars()) {
            Some(n) => self.s.name_classes(n),
            None => std::rc::Rc::new(vec![]),
        };
        if all.len() <= MAX_STORE_CLASSES {
            return Some((all.to_vec(), true));
        }
        let site = crate::ids::Site::new(self.s.sid, pc);
        let (lo, hi) = if let Some(ps) = self.s.ctx.prop_sites_in.get(&site) {
            (ps.layout_id, ps.hi_layout_id)
        } else if let Some(&cls) = self.val_cls.get(&recv.v) {
            (cls, cls)
        } else if recv.v == self.st.first().map(|x| x.v)? || self.this_vals.contains(&recv.v) {
            let ctx = self.s.ctx;
            let sid = self.s.sid;
            if let Some(li) = ctx.this_layouts_in.get(&sid) {
                (li.layout_id, li.hi_layout_id)
            } else if let Some(si) = ctx
                .stamp_ctors_in
                .get(&sid)
                .or_else(|| ctx.deleg_restamps_in.get(&sid))
            {
                (si.layout_id, si.layout_id)
            } else {
                let ctors = ctx.facts.ctor_publish.get(&sid)?;
                // A construction delegate (`ctor_publish`): the layouts
                // of the constructors whose objects it may be building.
                let keys: Vec<u32> = ctors
                    .iter()
                    .filter_map(|c| Some(ctx.stamp_ctors_in.get(c)?.layout_id + 1))
                    .collect();
                let some: Vec<(u32, TagSet)> = all
                    .iter()
                    .copied()
                    .filter(|(k, _)| keys.contains(k))
                    .collect();
                return Some((some, false));
            }
        } else {
            return None;
        };
        let some: Vec<(u32, TagSet)> = all
            .iter()
            .copied()
            .filter(|&(k, _)| k > lo && k <= hi + 1)
            .collect();
        Some((some, false))
    }

    /// A property access with no site row, through a receiver the analysis
    /// hints one class for (`val_cls`: advisory, from
    /// `arg_cls` and `field_cls_sites`): that class's field, from its
    /// layout row (`this_layouts_in`). The hint is unchecked; the typed
    /// access guards it, reading through the IC on a miss.
    pub(super) fn hinted_site(
        &mut self,
        recv: mir::Value,
        name: mir::entity::AtomId,
    ) -> Option<TypedSite> {
        let cls = *self.val_cls.get(&recv)?;
        let n = self.s.names?.lookup(self.mm.atoms[name].chars())?;
        let (slot, claim) = self.s.layout_field(cls, n)?;
        self.site_for(cls, cls, slot, claim, true, name)
    }

    /// The accessor an accessor-site property access calls
    /// (`accessor_sites`, the modeled `Object.defineProperty` accessors):
    /// the site's resolved getter or setter, or, for a name registered as
    /// an accessor somewhere, `None` (probed without a static target).
    /// `None` outright where neither applies.
    pub(super) fn accessor_site(
        &self,
        pc: Pc,
        a: mir::entity::AtomId,
        set: bool,
    ) -> Option<Option<ScriptId>> {
        let facts = &self.s.ctx.facts;
        match facts.accessor_sites.get(&self.site(pc)) {
            Some(&(k, kind)) if kind == u8::from(set) => return Some(Some(k)),
            _ => {}
        }
        let n = self.s.names?.lookup(self.mm.atoms[a].chars())?;
        facts.accessor_names.contains(&n).then_some(None)
    }

    /// `recv.a` at an accessor site: the receiver's shape probed in the
    /// runtime's accessor-call cache (`accessor.probe`), the getter it
    /// finds called with `recv` as `this` (directly, where the site
    /// resolves it); any other receiver reads through the IC.
    /// The name a property key slot is, where MIR knows it is one string
    /// constant that is not an array index. (An index string named so would
    /// be right too, the engine canonicalizing its key; it is left to the
    /// element paths, which are faster for it.)
    pub(super) fn const_key_atom(&self, key: Slot) -> Option<mir::entity::AtomId> {
        // Through the boxing a local's store adds.
        let mut v = key.v;
        let a = loop {
            if let MType::Str(info) = self.f.ty(v) {
                if let Some(a) = info.atom {
                    break a;
                }
            }
            match self.f.values[v].def {
                mir::func::ValueDef::Result(i, 0) => match self.f.insts[i].op {
                    Opcode::Box | Opcode::Weaken => v = self.f.insts[i].args[0],
                    Opcode::ConstStr(a) => break a,
                    _ => return None,
                },
                _ => return None,
            }
        };
        (!crate::likelier::types::is_index_name(self.mm.atoms[a].chars())).then_some(a)
    }

    /// `GetProp` of name `a` off `recv` at `pc` (`next` its successor): an
    /// element read whose key is a string constant builds this too.
    pub(super) fn get_prop_op(
        &mut self,
        pc: Pc,
        next: Pc,
        a: mir::entity::AtomId,
        recv: Slot,
    ) -> R<()> {
        // The elided arguments object's length: the actual count.
        if self.s.args_reads.as_ref().is_some_and(|r| r.contains(&pc)) {
            let n = self.inst(
                Opcode::ArgsLength,
                vec![],
                Some(MType::i32_range(0, i64::from(i32::MAX))),
            );
            self.push(n, Ty::I32);
            return Ok(());
        }
        // A typed array's `length` is not folded to its length slot: the kind
        // guard proves the class, not that no own `length` shadows the builtin
        // getter or that the prototype's getter is still the engine's. The
        // generic read (`PureLengthRead`) decides that.
        if self.mm.atoms[a].chars() == "length".encode_utf16().collect::<Vec<u16>>().as_slice()
            && self.length_op(recv)
        {
            return Ok(());
        }
        if self.ctor_get(next, a, recv) {
            return Ok(());
        }
        if self.accessor_get(pc, next, a, recv) {
            return Ok(());
        }
        if self.method_get(next, a, recv) {
            return Ok(());
        }
        let site = self
            .typed_site(pc, a)
            .or_else(|| self.hinted_site(recv.v, a))
            .or_else(|| self.named_site(recv, a));
        if let Some((o, site)) = site.and_then(|site| self.proven_recv(recv, &site)) {
            let r = self
                .js_dirty_exits(
                    Opcode::LoadField(a),
                    vec![o],
                    Some(site.claim_ty()),
                    next,
                    None,
                )
                .unwrap();
            // Under TYPES the read is of the field's predicted type: it
            // keeps those tags, and `guard_result` then unboxes without a
            // guard. Without TYPES the claim is guarded at the def.
            let (t, tags) = if site.types {
                (site.claim_ty(), site.load_tags)
            } else {
                (MType::VAL_TOP, TagSet::ALL)
            };
            let r = self.weaken(r, t);
            self.push(r, Ty::Val(tags));
            self.guard_result(site.claim, next, false);
        } else if let Some(site) = site {
            // The predicted layout: guard the receiver's class
            // locally, then load the field (§4.3). A receiver of
            // another layout reads through the IC instead of
            // exiting: a prediction that is wrong for some receivers
            // must not send the rest of the activation to baseline
            // every time. The claim is guarded after the join. The
            // IC arm keeps facts, a kill exiting; where it cannot,
            // it is a fence, and `Obj` slots meet as `ObjHint`.
            // A data read (`getprop.data`) kills nothing; a generic
            // one fences.
            let data = self.data_get_ok(a);
            if !data {
                self.demote_for_generic_arm(1);
            }
            let x = self.boxed(recv);
            let join = self.new_block();
            let jr = self.f.add_param(join, MType::VAL_TOP);
            let generic = self.new_block();
            let o = self.guard_layout_or(x, &site, generic);
            let r = self
                .js_dirty_exits(
                    Opcode::LoadField(a),
                    vec![o],
                    Some(site.claim_ty()),
                    next,
                    None,
                )
                .unwrap();
            let r = self.weaken(r, MType::VAL_TOP);
            self.term(
                Opcode::Jump,
                vec![],
                vec![Edge {
                    block: join,
                    args: vec![EdgeArg::Value(r)],
                }],
            );
            self.at(generic);
            let r = if data {
                self.data_get(a, x)
            } else {
                self.js(Opcode::JsGetProp(a), vec![x], MType::VAL_TOP)
            };
            self.term(
                Opcode::Jump,
                vec![],
                vec![Edge {
                    block: join,
                    args: vec![EdgeArg::Value(r)],
                }],
            );
            self.at(join);
            self.push(jr, Ty::Val(TagSet::ALL));
            self.guard_result(site.claim, next, false);
        } else {
            let x = self.boxed(recv);
            let r = if self.data_get_ok(a) {
                self.data_get(a, x)
            } else {
                self.js(Opcode::JsGetProp(a), vec![x], MType::VAL_TOP)
            };
            // `Math.<fn>`, for a typed call (`math_call`).
            if self
                .gname_vals
                .get(&recv.v)
                .is_some_and(|&g| self.atom_is(g, "Math"))
            {
                if let Some(m) = math_fn_named(&std::string::String::from_utf16_lossy(
                    self.mm.atoms[a].chars(),
                )) {
                    self.math_fns.insert(r, m);
                }
            }
            self.push(r, Ty::Val(TagSet::ALL));
            let claim = self.s.ctx.facts.field_sites.get(&self.site(pc)).copied();
            self.guard_result(claim.unwrap_or_default(), next, false);
        }
        Ok(())
    }

    /// `SetProp` (`strict`: `StrictSetProp`) of `v` into name `a` of
    /// `recv` at `pc` (`next` its successor), the value pushed back: an
    /// element write whose key is a string constant builds this too.
    pub(super) fn set_prop_op(
        &mut self,
        pc: Pc,
        next: Pc,
        a: mir::entity::AtomId,
        recv: Slot,
        v: Slot,
        strict: bool,
    ) {
        if self.ctor_set(next, a, recv, v) {
            self.repush(v);
            return;
        }
        if self.accessor_set(pc, a, recv, v, strict) {
            self.repush(v);
            return;
        }
        self.store_mask = self.field_claim(pc, a, recv);
        // A field of the predicted layout: a value of its predicted
        // type (statically) keeps TYPES as a typed store; another
        // goes through the IC, whose store keeps the bit by the
        // value's tag or leaves it to the engine, which clears it.
        // A value not statically of the field's type still stores
        // to the slot, as a store without the TYPES claim: its
        // lowering keeps the bit by the value's tag, or leaves the
        // store to the engine, which clears it.
        let vtags = v.ty.tags();
        let site = self
            .typed_site(pc, a)
            .or_else(|| self.hinted_site(recv.v, a))
            .or_else(|| self.named_site(recv, a))
            .map(|mut s| {
                s.types &= vtags.is_nonempty_subset_of(s.store_tags);
                s
            });
        let proven = site.and_then(|site| self.proven_recv(recv, &site));
        // The IC arm of an unproven typed store keeps facts, a kill
        // exiting; where it cannot, it is a fence the two arms meet
        // after: `Obj` slots (and the value, pushed back after it)
        // go in as `ObjHint`.
        let v = if site.is_some() && proven.is_none() {
            self.demote_value_for_generic_arm(v)
        } else {
            v
        };
        match site {
            Some(_) if proven.is_some() => {
                let (mut o, site) = proven.unwrap();
                if !site.types {
                    // Its TYPES claim is not the store's to keep.
                    if let MType::Obj(mut info) = self.f.ty(o) {
                        if let Some(c) = info.layout.as_mut() {
                            c.types = false;
                        }
                        o = self.weaken(o, MType::Obj(info));
                    }
                }
                let mut x = self.boxed(v);
                if site.types {
                    x = self.weaken(x, MType::val(site.store_tags));
                }
                self.js_dirty_exits(Opcode::StoreField(a), vec![o, x], None, next, Some(v));
            }
            Some(site) => {
                // As for a typed read: another layout's receiver
                // stores through the IC rather than exiting.
                let r = self.boxed(recv);
                let join = self.new_block();
                let generic = self.new_block();
                let o = self.guard_layout_or(r, &site, generic);
                let mut x = self.boxed(v);
                if site.types {
                    x = self.weaken(x, MType::val(site.store_tags));
                }
                self.js_dirty_exits(Opcode::StoreField(a), vec![o, x], None, next, Some(v));
                self.term(Opcode::Jump, vec![], vec![Self::goto(join)]);
                self.at(generic);
                let y = self.boxed(v);
                self.set_prop(a, strict, r, y, v);
                self.term(Opcode::Jump, vec![], vec![Self::goto(join)]);
                self.at(join);
            }
            None => {
                let (x, y) = (self.boxed(recv), self.boxed(v));
                self.set_prop(a, strict, x, y, v);
            }
        }
        self.repush(v);
    }

    pub(super) fn accessor_get(
        &mut self,
        pc: Pc,
        next: Pc,
        a: mir::entity::AtomId,
        recv: Slot,
    ) -> bool {
        let Some(target) = self.accessor_site(pc, a, false) else {
            return false;
        };
        let x = self.boxed(recv);
        let r = self.accessor_call(
            a,
            false,
            target,
            vec![x],
            MType::VAL_TOP,
            Opcode::JsGetProp(a),
        );
        self.push(r, Ty::Val(TagSet::ALL));
        let claim = self.s.ctx.facts.field_sites.get(&self.site(pc)).copied();
        self.guard_result(claim.unwrap_or_default(), next, false);
        true
    }

    /// `recv.a = v` at an accessor site: the setter the probe finds
    /// called with `recv` and `v`; any other receiver stores through the
    /// IC. The value stays on the stack.
    pub(super) fn accessor_set(
        &mut self,
        pc: Pc,
        a: mir::entity::AtomId,
        recv: Slot,
        v: Slot,
        strict: bool,
    ) -> bool {
        let Some(target) = self.accessor_site(pc, a, true) else {
            return false;
        };
        let (x, y) = (self.boxed(recv), self.boxed(v));
        self.accessor_call(
            a,
            true,
            target,
            vec![x, y],
            MType::VAL_TOP,
            Opcode::JsSetProp(a, strict),
        );
        true
    }

    /// A generic numeric op or compare (`js`) on `args`, as `prim.*`: an
    /// operand whose conversion would call user code (`valueOf`,
    /// `toString`, `Symbol.toPrimitive` of its own) exits here, before the
    /// op, for baseline to do it; the op's own TypeError or RangeError
    /// takes the throw exit.
    pub(super) fn prim_or_js(
        &mut self,
        op: Opcode,
        args: Vec<mir::Value>,
        out: MType,
    ) -> mir::Value {
        let Some(p) = mir::ops::PrimOp::of(&op) else {
            return self.js(op, args, out);
        };
        let ok = self.new_block();
        let r = self.f.add_param(ok, out);
        let fail = self.exit_block(false);
        let err = self.exit_block(true);
        self.term(
            Opcode::Prim(p),
            args,
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

    /// `x.a = y` (`post` the value, left on the stack) through the IC: a
    /// set that runs no code (`setprop.data`, MIR-MEMORY.md §1) where no
    /// known setter has the name and a demotion can exit after it, a set
    /// that would run code exiting here, before the op, for baseline to
    /// do it; else the generic set.
    pub(super) fn set_prop(
        &mut self,
        a: mir::entity::AtomId,
        strict: bool,
        x: mir::Value,
        y: mir::Value,
        post: Slot,
    ) {
        if self.data_get_ok(a) {
            if let Some(dirty) = self.dirty_exit(None, Some(post)) {
                let ok = self.new_block();
                let fail = self.exit_block(false);
                let err = self.exit_block(true);
                let (inst, _) = self.f.add_inst(
                    self.cur,
                    Opcode::SetPropData(a),
                    vec![x, y],
                    &[],
                    vec![Self::goto(ok), dirty, Self::goto(fail), Self::goto(err)],
                );
                self.attach_targets(inst);
                self.live = false;
                self.at(ok);
                return;
            }
        }
        self.js_void_keep(Opcode::JsSetProp(a, strict), vec![x, y], post);
    }

    /// The probe-and-call diamond of `accessor_get`/`accessor_set`:
    /// `vals` the receiver (and the value); `generic` the IC op on a miss.
    /// A getter's result, or (for a setter) the call's, unused.
    pub(super) fn accessor_call(
        &mut self,
        a: mir::entity::AtomId,
        set: bool,
        target: Option<ScriptId>,
        vals: Vec<mir::Value>,
        out: MType,
        generic: Opcode,
    ) -> mir::Value {
        // Arms that fence meet at `join`: `Obj` slots go in as `ObjHint`.
        self.demote_for_generic_arm(1);
        let join = self.new_block();
        let jr = self.f.add_param(join, out);
        let (hit, miss) = (self.new_block(), self.new_block());
        let callee = self.f.add_param(hit, MType::val(TagSet::OBJECT));
        self.term(
            Opcode::AccessorProbe(a, set),
            vec![vals[0]],
            vec![
                Edge {
                    block: hit,
                    args: vec![EdgeArg::Out(0)],
                },
                Self::goto(miss),
            ],
        );
        self.at(hit);
        let mut call = vec![callee];
        call.extend(&vals);
        self.likely_targets = match target {
            Some(k) if self.s.direct_ok(k) => vec![k],
            _ => vec![],
        };
        let r = if set {
            // The stack after the op holds the stored value, not the
            // setter's result.
            self.js_void_keep(
                Opcode::Call,
                call,
                Slot {
                    v: vals[1],
                    ty: Ty::Val(TagSet::ALL),
                },
            );
            vals[1]
        } else {
            self.js(Opcode::Call, call, out)
        };
        self.likely_targets.clear();
        self.term(
            Opcode::Jump,
            vec![],
            vec![Edge {
                block: join,
                args: vec![EdgeArg::Value(r)],
            }],
        );
        self.at(miss);
        let r = if set {
            self.js_void_keep(
                generic,
                vals.clone(),
                Slot {
                    v: vals[1],
                    ty: Ty::Val(TagSet::ALL),
                },
            );
            vals[1]
        } else {
            self.js(generic, vals, out)
        };
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

    /// A field of a receiver proven with TYPES whose layouts all type it
    /// but predict no slot for it (added outside the constructors): read
    /// or written through an IC for the slot, of its predicted type.
    /// Registered in the module's layouts as a named field.
    pub(super) fn named_site(
        &mut self,
        recv: Slot,
        name: mir::entity::AtomId,
    ) -> Option<TypedSite> {
        let Ty::Obj(keys, true, _) = recv.ty else {
            return None;
        };
        let n = self.s.names?.lookup(self.mm.atoms[name].chars())?;
        let (lo, hi) = (keys.lo.get(), keys.hi.get());
        let mut masks = vec![];
        for k in lo..=hi {
            if self.s.layout_field(k, n).is_some() {
                return None;
            }
            let c = *self
                .s
                .ctx
                .layout_field_types_in
                .get(&crate::ids::LayoutKey::new(k).stamp())?
                .get(&n)?;
            if c.is_none() {
                return None;
            }
            masks.push(claim_tags(c));
        }
        for (i, k) in (lo..=hi).enumerate() {
            let def = mir::module::FieldDef {
                name,
                claim: MType::val(masks[i]),
            };
            let l = self
                .mm
                .layouts
                .entry(crate::ids::LayoutKey::new(k))
                .or_default();
            if l.field(name).is_some() {
                return None;
            }
            match l.named_field(name) {
                None => l.named.push(def),
                Some(f) if *f == def => {}
                Some(_) => return None,
            }
        }
        Some(TypedSite {
            lo,
            hi,
            types: true,
            claim: crate::facts::Claim::NONE,
            load_tags: masks.iter().fold(TagSet::NONE, |a, &m| a.union(m)),
            store_tags: masks.iter().fold(TagSet::ALL, |a, &m| a.intersect(m)),
            slotted: false,
        })
    }

    pub(super) fn site_for(
        &mut self,
        lo: u32,
        hi: u32,
        slot: u32,
        claim_in: crate::facts::Claim,
        shallow_possible: bool,
        name: mir::entity::AtomId,
    ) -> Option<TypedSite> {
        // TYPES (§4.6): each layout's predicted type for the field, where
        // the receivers can carry the bit and every layout predicts one.
        let n = self
            .s
            .names
            .and_then(|ns| ns.lookup(self.mm.atoms[name].chars()));
        let masks: Vec<TagSet> = (lo..=hi)
            .filter_map(|k| {
                let c = *self
                    .s
                    .ctx
                    .layout_field_types_in
                    .get(&crate::ids::LayoutKey::new(k).stamp())?
                    .get(&n?)?;
                (!c.is_none()).then(|| claim_tags(c))
            })
            .collect();
        let types = shallow_possible && masks.len() == (hi - lo + 1) as usize;
        let load_tags = masks.iter().fold(TagSet::NONE, |a, &m| a.union(m));
        let store_tags = masks.iter().fold(TagSet::ALL, |a, &m| a.intersect(m));
        let site = TypedSite {
            lo,
            hi,
            types,
            claim: claim_in,
            load_tags,
            store_tags,
            slotted: true,
        };
        let slot = usize::try_from(slot).unwrap();
        for (i, k) in (site.lo..=site.hi).enumerate() {
            let claim = if types {
                MType::val(masks[i])
            } else {
                MType::VAL_TOP
            };
            let l = self
                .mm
                .layouts
                .entry(crate::ids::LayoutKey::new(k))
                .or_default();
            if l.fields.len() <= slot {
                l.fields.resize(slot + 1, None);
            }
            match &l.fields[slot] {
                None => {
                    l.fields[slot] = Some(mir::module::FieldDef { name, claim });
                }
                Some(f) if f.name == name && f.claim == claim => {}
                Some(_) => return None,
            }
        }
        Some(site)
    }

    /// `recv.a` at a predicted-method site (`method_sites`), the read before
    /// `next`: on a receiver of the site's layouts, CLOSED (no own property
    /// outside the row, which lacks the name), the function its method
    /// cell holds for the receiver's prototype (`method.load`); else
    /// through the IC, whose value must be a function of the predicted
    /// script or the read exits after it. The result is typed that
    /// function either way, so a call through it needs no callee guard.
    ///
    /// Where the method's own `this` claim (its entry guard) covers the
    /// site's layouts, the receiver is guarded to it first, exiting before
    /// the read on a receiver of another layout (one the method's guard
    /// would refuse): its copies (the call's `this`) are then of that type,
    /// and the method, inlined, is built for it, with no guard of its own.
    pub(super) fn method_get(&mut self, next: Pc, a: mir::entity::AtomId, recv: Slot) -> bool {
        let Some(&ms) = self.s.ctx.method_sites_in.get(&self.site(self.pc)) else {
            return false;
        };
        // The IC arm exits after the read, with its value pushed.
        let depth = self.st.len() + 1 - self.frame_len();
        if self.s.depths.at(next) != Some(u32::try_from(depth).unwrap()) {
            return false;
        }
        let k = ms.script;
        self.mm.script_addrs.insert(k, ms.script_addr);
        let fobj = MType::Obj(ObjInfo::kind(ObjKind::Function(Some(k))));
        let ft = mir::ops::box_type(&fobj).expect("an object boxes");
        let keys = KeyRange {
            lo: crate::ids::LayoutKey::new(ms.layout_id),
            hi: crate::ids::LayoutKey::new(ms.hi_layout_id),
        };
        let types = (ms.layout_id..=ms.hi_layout_id).all(|kk| self.s.types_bit(kk) != 0);
        let typed_recv = self
            .s
            .ctx
            .facts
            .this_layouts
            .get(&k)
            .is_some_and(|&(lo, hi)| KeyRange { lo, hi }.contains(&keys));
        let data = self.data_get_ok(a);
        if !data {
            self.demote_for_generic_arm(1);
        }
        let x = self.boxed(recv);
        let join = self.new_block();
        let jr = self.f.add_param(join, ft);
        let generic = self.new_block();
        // The receiver, of the site's layouts: of the method's claim, or
        // exiting here; else any other reads through the IC.
        let (gt, gs, miss) = if typed_recv {
            (types, true, self.exit_block(false))
        } else {
            (false, false, generic)
        };
        let ob = self.new_block();
        let o = self.f.add_param(ob, MType::OBJ_TOP);
        self.term(
            Opcode::GuardUnbox(UnboxKind::Obj),
            vec![x],
            vec![
                Edge {
                    block: ob,
                    args: vec![EdgeArg::Out(0)],
                },
                Self::goto(miss),
            ],
        );
        self.at(ob);
        let rty = Ty::Obj(keys, gt, gs);
        let lb = self.new_block();
        let lo = self.f.add_param(lb, rty.mir());
        self.term(
            Opcode::GuardLayout {
                keys,
                types: gt,
                slots: gs,
                closed: false,
            },
            vec![o],
            vec![
                Edge {
                    block: lb,
                    args: vec![EdgeArg::Out(0)],
                },
                Self::goto(miss),
            ],
        );
        self.at(lb);
        if typed_recv {
            // The receiver's copies on the operand stack (the call's
            // `this`) are this object, of this type, known as boxed as `x`;
            // the frame's (a local's) stay as they are, boxed.
            let base = self.frame_len();
            for s in self.st[base..].iter_mut() {
                if s.v == recv.v {
                    *s = Slot { v: lo, ty: rty };
                }
            }
            self.boxed_as.insert(lo, x);
        }
        // The cell's arm (`method.load` tests CLOSED itself).
        let mb = self.new_block();
        let mv = self.f.add_param(mb, fobj);
        self.term(
            Opcode::MethodLoad {
                cell: ms.cell_addr,
                atom: a,
                script: k,
            },
            vec![lo],
            vec![
                Edge {
                    block: mb,
                    args: vec![EdgeArg::Out(0)],
                },
                Self::goto(generic),
            ],
        );
        self.at(mb);
        let bv = self.inst(Opcode::Box, vec![mv], Some(ft));
        self.term(
            Opcode::Jump,
            vec![],
            vec![Edge {
                block: join,
                args: vec![EdgeArg::Value(bv)],
            }],
        );
        // The IC's arm, its value the predicted function or an exit.
        self.at(generic);
        let r = if data {
            self.data_get(a, x)
        } else {
            self.js(Opcode::JsGetProp(a), vec![x], MType::VAL_TOP)
        };
        let mut st = self.st.clone();
        st.push(Slot {
            v: r,
            ty: Ty::Val(TagSet::ALL),
        });
        let saved = (self.cur, self.live);
        let fail = self.new_block();
        self.at(fail);
        let ops = self.exit_operands(next, &st);
        let eop = self.exit_op(next, false);
        self.term(eop, ops, vec![]);
        (self.cur, self.live) = saved;
        let gb = self.new_block();
        let go = self.f.add_param(gb, MType::OBJ_TOP);
        self.term(
            Opcode::GuardUnbox(UnboxKind::Obj),
            vec![r],
            vec![
                Edge {
                    block: gb,
                    args: vec![EdgeArg::Out(0)],
                },
                Self::goto(fail),
            ],
        );
        self.at(gb);
        let sb = self.new_block();
        let so = self.f.add_param(sb, fobj);
        self.term(
            Opcode::GuardScript(k),
            vec![go],
            vec![
                Edge {
                    block: sb,
                    args: vec![EdgeArg::Out(0)],
                },
                Self::goto(fail),
            ],
        );
        self.at(sb);
        let bv = self.inst(Opcode::Box, vec![so], Some(ft));
        self.term(
            Opcode::Jump,
            vec![],
            vec![Edge {
                block: join,
                args: vec![EdgeArg::Value(bv)],
            }],
        );
        self.at(join);
        self.push(jr, Ty::Fn(k));
        true
    }
}
