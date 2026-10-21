//! inline: methods of `Run`, split out of `calls.rs` unchanged.

use super::*;

impl<'s, 'a> Run<'s, 'a> {
    /// A call whose predicted targets are inlined (§5.5): the callee
    /// guarded to each target's script in turn, each target's MIR spliced
    /// in on its hit; the ordinary call when none matches. Returns the
    /// result, at the continuation. With `this_raw` (an object under
    /// construction, `ctor_pass`, where `this_ctor`; else a published one
    /// of a proven type), the callees receive it as their `this`, built for
    /// it.
    pub(in crate::wasm::mir::build) fn inline_call_this(
        &mut self,
        targets: &[(ScriptId, std::rc::Rc<super::super::super::inline::Callee>)],
        vals: &[mir::Value],
        fallback: Option<(Opcode, Vec<mir::Value>)>,
        this_raw: Option<mir::Value>,
        this_ctor: bool,
    ) -> mir::Value {
        // Facts are fixed: with every kill in the callees exiting (none
        // fenced), a kill leaves for baseline at the call's next pc, and
        // this frame's `Obj` slots keep their facts through the splice.
        // A fenced callee's continue as `ObjHint`.
        let keep = !targets.iter().any(|(_, c)| c.fenced) && self.keepable(1);
        if !keep {
            self.demote_objs();
        }
        let dirty = if keep {
            self.dirty_exit_as(Some(MType::VAL_TOP), None, true)
        } else {
            None
        };
        let join = self.new_block();
        let result = self.f.add_param(join, MType::VAL_TOP);
        let err = self.exit_block(true);
        let generic = self.new_block();
        let obj_b = self.new_block();
        let obj = self.f.add_param(obj_b, MType::OBJ_TOP);
        self.term(
            Opcode::GuardUnbox(UnboxKind::Obj),
            vec![vals[0]],
            vec![
                Edge {
                    block: obj_b,
                    args: vec![EdgeArg::Out(0)],
                },
                Self::goto(generic),
            ],
        );
        self.at(obj_b);
        for (k, callee) in targets {
            let hit = self.new_block();
            let kt = MType::Obj(ObjInfo::kind(ObjKind::Function(Some(*k))));
            let kobj = self.f.add_param(hit, kt);
            let miss = self.new_block();
            self.term(
                Opcode::GuardScript(*k),
                vec![obj],
                vec![
                    Edge {
                        block: hit,
                        args: vec![EdgeArg::Out(0)],
                    },
                    Self::goto(miss),
                ],
            );
            self.at(hit);
            let nformals = callee.f.frame.formals as usize;
            let mut operands = vec![kobj, this_raw.unwrap_or(vals[1])];
            let undef = self.const_val(ConstVal::Undefined);
            // A callee reading its actuals gets them all.
            let argc = callee
                .reads_actuals
                .then(|| u32::try_from(vals.len() - 2).unwrap());
            let n = nformals.max(argc.map_or(0, |a| a as usize));
            for i in 0..n {
                operands.push(vals.get(2 + i).copied().unwrap_or(undef));
            }
            let saved_mm = self.mm.clone();
            let saved_frames = self.f.inline_frames.len();
            let frame_this = this_raw.map(|_| vals[1]);
            match super::super::super::inline::splice(
                &mut self.mm,
                &mut self.f,
                callee,
                0,
                hit,
                &operands,
                None,
                frame_this,
                argc,
                join,
                dirty.clone(),
                err,
            ) {
                Ok(()) => {
                    self.mm.script_addrs.insert(*k, callee.mm.script_addrs[k]);
                }
                Err(_) => {
                    // Not this one after all: the hit takes the ordinary call
                    // (the guard stays, so its script's address does too).
                    self.mm = saved_mm;
                    self.mm.script_addrs.insert(*k, callee.mm.script_addrs[k]);
                    self.f.inline_frames.truncate(saved_frames);
                    self.term(Opcode::Jump, vec![], vec![Self::goto(generic)]);
                }
            }
            self.at(miss);
        }
        self.term(Opcode::Jump, vec![], vec![Self::goto(generic)]);
        self.at(generic);
        let e = Edge {
            block: join,
            args: vec![EdgeArg::Out(0)],
        };
        // A wrapper constructor's forward of its `this` under construction
        // to the target the analysis resolved for this copy: another target
        // (`initialize` replaced) is baseline's to call, which keeps the
        // object from every generic op (MIR-MEMORY.md §6).
        if this_ctor {
            let ex = self.exit_block(false);
            self.term(Opcode::Jump, vec![], vec![Self::goto(ex)]);
            self.at(join);
            return result;
        }
        let (op, args) = fallback.unwrap_or((Opcode::Call, vals.to_vec()));
        match dirty {
            Some(d) => {
                // Not `term`: the clean edge is no fence.
                self.f
                    .add_inst(self.cur, op, args, &[], vec![e, d, Self::goto(err)]);
                self.live = false;
            }
            None => self.term(op, args, vec![e.clone(), e, Self::goto(err)]),
        }
        self.at(join);
        result
    }

    /// `new F(args…)` of the site's one constructor inlined (§5.5): with
    /// the callee that script's function, `this` is made as a direct
    /// construct makes it (`create_this`), the body is spliced with its
    /// frame's new.target, and the result is the body's value if an
    /// object, else `this`; any other callee takes the generic construct.
    /// Operands `callee, is_constructing, args…, new.target`.
    pub(in crate::wasm::mir::build) fn inline_construct(
        &mut self,
        k: ScriptId,
        callee: &std::rc::Rc<super::super::super::inline::Callee>,
        vals: &[mir::Value],
        nslots: u32,
        word: u32,
    ) -> mir::Value {
        self.demote_objs();
        let nt = vals[vals.len() - 1];
        let join = self.new_block();
        let result = self.f.add_param(join, MType::val(TagSet::OBJECT));
        let err = self.exit_block(true);
        let generic = self.new_block();
        let obj_b = self.new_block();
        let obj = self.f.add_param(obj_b, MType::OBJ_TOP);
        self.term(
            Opcode::GuardUnbox(UnboxKind::Obj),
            vec![vals[0]],
            vec![
                Edge {
                    block: obj_b,
                    args: vec![EdgeArg::Out(0)],
                },
                Self::goto(generic),
            ],
        );
        self.at(obj_b);
        let hit = self.new_block();
        let kt = MType::Obj(ObjInfo::kind(ObjKind::Function(Some(k))));
        let kobj = self.f.add_param(hit, kt);
        self.term(
            Opcode::GuardScript(k),
            vec![obj],
            vec![
                Edge {
                    block: hit,
                    args: vec![EdgeArg::Out(0)],
                },
                Self::goto(generic),
            ],
        );
        self.at(hit);
        // Not a constructor (an arrow function, a method): the generic
        // construct throws.
        let is_ctor = self.inst(Opcode::FnIsCtor, vec![kobj], Some(MType::Bool));
        let ctor = self.new_block();
        self.term(
            Opcode::Br,
            vec![is_ctor],
            vec![Self::goto(ctor), Self::goto(generic)],
        );
        self.at(ctor);
        // A layout constructor that is its own new.target (`new F()`):
        // `this` from its `prototype`, read by name (its fail exits before
        // the `new`), an allocation (MIR-MEMORY.md §6). Else a fence: the
        // next pc's value is the construct's, not `this`, so a kill here
        // has nowhere to exit to.
        let proto_atom = self
            .mm
            .intern_atom(&"prototype".encode_utf16().collect::<Vec<u16>>());
        let new_this =
            nt == vals[0] && self.ctor_of_word(k, word).is_some() && self.data_get_ok(proto_atom);
        let mut callee = callee.clone();
        // The constructor's leading stores of its formals, made with the
        // allocation (`new_this.init`): the body is built for the object
        // with those fields, and skips them.
        let prefilled = if new_this {
            self.ctor_prefill(k, word, &vals[2..vals.len() - 1])
        } else {
            None
        };
        if let Some((key, t, xs, c)) = prefilled {
            self.inline_sites = (self.inline_sites + c.sites).saturating_sub(callee.sites);
            callee = c;
            let proto = self.data_get(proto_atom, vals[0]);
            let n = u32::try_from(xs.len()).unwrap();
            let ok = self.new_block();
            let this_n = self
                .f
                .add_param(ok, Ty::Ctor(key, FieldSet::prefix(n), t).mir());
            let (fail, err) = (self.exit_block(false), self.exit_block(true));
            let op = Opcode::NewThisInit {
                nslots,
                word,
                key: crate::ids::LayoutKey::new(key),
                types: t,
                n,
            };
            let mut operands = vec![kobj, proto];
            operands.extend(xs);
            self.term(
                op,
                operands,
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
            let this = self.inst(
                Opcode::Box,
                vec![this_n],
                Some(mir::ops::box_type(&self.f.ty(this_n)).expect("objects box")),
            );
            let this = self.weaken(this, MType::val(TagSet::OBJECT));
            return self.splice_construct(
                k, &callee, vals, nt, kobj, this, this_n, join, result, err, generic, nslots, word,
            );
        }
        let this = if new_this {
            let proto = self.data_get(proto_atom, vals[0]);
            self.js_static(
                Opcode::NewThis(nslots, word),
                vec![kobj, proto],
                MType::val(TagSet::OBJECT),
            )
        } else {
            self.js_fence(
                Opcode::CreateThis(nslots, word),
                vec![vals[0], nt],
                MType::val(TagSet::OBJECT),
            )
        };
        // The object `create_this` made carries `word` (§2.3): under
        // construction for the constructor's own layout, with no field
        // yet. The body is built for that `this`, whose adds are then
        // `init_field`s and whose methods see its construction state.
        let mut this_op = this;
        if let Some((key, t)) = self.ctor_of_word(k, word) {
            if let Some(c) =
                self.s
                    .callee_ctx(k, &[], Some((key, FieldSet::EMPTY, t)), None, self.admitted)
            {
                self.inline_sites = (self.inline_sites + c.sites).saturating_sub(callee.sites);
                let o = self.assert_guard(
                    Opcode::GuardUnbox(UnboxKind::Obj),
                    vec![this],
                    MType::OBJ_TOP,
                );
                let g = Opcode::GuardCtor {
                    key: crate::ids::LayoutKey::new(key),
                    n: FieldSet::EMPTY,
                    types: t,
                };
                this_op = self.assert_guard(g, vec![o], Ty::Ctor(key, FieldSet::EMPTY, t).mir());
                callee = c;
            }
        }
        self.splice_construct(
            k, &callee, vals, nt, kobj, this, this_op, join, result, err, generic, nslots, word,
        )
    }

    /// `inline_construct`'s splice of the constructor `callee` (`k`) for
    /// `this_op` (`this` boxed), and its result rule: the body's value if
    /// an object, else `this`, at `join` (`result`); the generic construct
    /// where the splice fails. Returns `result`.
    #[allow(clippy::too_many_arguments)]
    pub(in crate::wasm::mir::build) fn splice_construct(
        &mut self,
        k: ScriptId,
        callee: &std::rc::Rc<super::super::super::inline::Callee>,
        vals: &[mir::Value],
        nt: mir::Value,
        kobj: mir::Value,
        this: mir::Value,
        this_op: mir::Value,
        join: mir::Block,
        result: mir::Value,
        err: mir::Block,
        generic: mir::Block,
        nslots: u32,
        word: u32,
    ) -> mir::Value {
        let nformals = callee.f.frame.formals as usize;
        let nargs = vals.len() - 3;
        let mut operands = vec![kobj, this_op];
        let undef = self.const_val(ConstVal::Undefined);
        // A constructor reading its actuals gets them all.
        let argc = callee.reads_actuals.then(|| u32::try_from(nargs).unwrap());
        for i in 0..nformals.max(argc.map_or(0, |a| a as usize)) {
            operands.push(if i < nargs { vals[2 + i] } else { undef });
        }
        // The body's value, then the construct's result rule.
        let ret = self.new_block();
        let r = self.f.add_param(ret, MType::VAL_TOP);
        let saved_mm = self.mm.clone();
        let saved_frames = self.f.inline_frames.len();
        let here = self.cur;
        match super::super::super::inline::splice(
            &mut self.mm,
            &mut self.f,
            callee,
            0,
            here,
            &operands,
            Some(nt),
            None,
            argc,
            ret,
            None,
            err,
        ) {
            Ok(()) => {
                self.mm.script_addrs.insert(k, callee.mm.script_addrs[&k]);
                self.live = false;
                self.at(ret);
                let t = self.new_block();
                self.term(
                    Opcode::GuardTags(TagSet::OBJECT),
                    vec![r],
                    vec![
                        Edge {
                            block: join,
                            args: vec![EdgeArg::Out(0)],
                        },
                        Self::goto(t),
                    ],
                );
                self.at(t);
                self.term(
                    Opcode::Jump,
                    vec![],
                    vec![Edge {
                        block: join,
                        args: vec![EdgeArg::Value(this)],
                    }],
                );
            }
            Err(_) => {
                self.mm = saved_mm;
                self.mm.script_addrs.insert(k, callee.mm.script_addrs[&k]);
                self.f.inline_frames.truncate(saved_frames);
                self.term(Opcode::Jump, vec![], vec![Self::goto(generic)]);
            }
        }
        self.at(generic);
        let e = Edge {
            block: join,
            args: vec![EdgeArg::Out(0)],
        };
        self.term(
            Opcode::Construct(nslots, word),
            vals.to_vec(),
            vec![e.clone(), e, Self::goto(err)],
        );
        self.at(join);
        result
    }
    /// The inlining admission (with its per-target rules): which of `sids` the site at `self.pc` inlines,
    /// each built by `build` where the site puts it, in order; `None` for
    /// none. Counts the site, and the sites its callees splice, against
    /// this function's budget.
    pub(in crate::wasm::mir::build) fn admit(
        &mut self,
        sids: &[ScriptId],
        construct: bool,
        build: &dyn Fn(
            &Shape<'a>,
            ScriptId,
            InlineCtx,
        ) -> Option<std::rc::Rc<super::super::super::inline::Callee>>,
    ) -> Option<Vec<(ScriptId, std::rc::Rc<super::super::super::inline::Callee>)>> {
        use crate::bytecode::TryNoteKind;
        let s = self.s;
        let pc = self.pc;
        let ictx = s.ictx;
        if sids.is_empty() {
            return None;
        }
        let local_nest =
            u32::try_from(s.loops.iter().filter(|(&h, &e)| h <= pc && pc < e).count()).unwrap();
        let root_in_loop = ictx.root_in_loop.unwrap_or(local_nest > 0);
        let max_depth = if root_in_loop {
            MAX_INLINE_DEPTH_LOOP
        } else {
            MAX_INLINE_DEPTH
        };
        if ictx.depth >= max_depth {
            return None;
        }
        let script = s.script;
        // The rule (no splicing into a function that uses `arguments`,
        // mapped formals or actuals), except where `arguments` only feeds
        // `T.apply(this, arguments)` forwards (`apply_fwd`, never made):
        // the frame's actuals are read in place, whatever is spliced.
        let needs_args_obj = (crate::wasm::translate::uses_arguments(script)
            && s.apply_fwd.is_none())
            || (script.has_mapped_args && script.nargs > 0)
            || crate::wasm::translate::uses_actual_args(script);
        if needs_args_obj || self.inline_sites >= ictx.budget {
            return None;
        }
        let bytes = |k: ScriptId| match s
            .ctx
            .source
            .object(crate::source::SourceObjectId::new(k.get()))
        {
            crate::source::SourceObject::Script(ks) => Some(ks),
            _ => None,
        };
        let n = self.f.insts.len();
        if n >= SPLICE_FUEL_INSTS {
            let small = sids
                .iter()
                .all(|&k| bytes(k).is_some_and(|ks| ks.bytecode.len() <= SMALL_SPLICE_BC));
            if !small || n >= SMALL_SPLICE_FUEL_INSTS {
                return None;
            }
        }
        if script.try_notes.iter().any(|t| {
            !matches!(t.kind, TryNoteKind::Loop) && pc >= t.start && pc < t.start + t.length
        }) {
            return None;
        }
        if sids.len() > MAX_INLINE_TARGETS {
            return None;
        }
        let cap = match (sids.len(), root_in_loop) {
            (1, true) => 200,
            (1, false) => 150,
            _ => crate::constants::MAX_INLINE_POLY_BYTES,
        };
        let nest = ictx.outer_nest + local_nest;
        let allow_callee_loops = nest == 0;
        let picked: Vec<ScriptId> = sids
            .iter()
            .copied()
            .filter(|&k| {
                let Some(ks) = bytes(k) else { return false };
                !ks.bytecode.is_empty()
                    && ks.bytecode.len() <= cap
                    && !(construct && ks.is_class_ctor)
                    && !(!construct && ks.is_class_ctor)
                    && (allow_callee_loops
                        || !ks.try_notes.iter().any(|t| t.kind == TryNoteKind::Loop))
            })
            .collect();
        if picked.is_empty() {
            return None;
        }
        if construct {
            if picked.len() != 1 {
                return None;
            }
            let room = crate::constants::CONSTRUCT_CLOSURE_CAP;
            let est = crate::wasm::mir::inline::splice_closure_cost(
                s.ctx,
                picked[0],
                max_depth - ictx.depth,
                room,
                Some(self.site(pc)),
            );
            if est > room {
                return None;
            }
        }
        // Depth-first: each target is a spliced segment, and may splice
        // what the budget has left once it and the segments before it
        // count.
        let mut used = self.inline_sites;
        let mut out = vec![];
        let mut admitted = None;
        for k in picked {
            let site = self.site(pc);
            let per_entry = bytes(k).is_some_and(layout::reads_actuals)
                && s.ctx
                    .facts
                    .apply_targets_in
                    .keys()
                    .any(|&(e, a)| e == site && a.script == k);
            let at = InlineCtx {
                depth: ictx.depth + 1,
                budget: ictx.budget.saturating_sub(used + 1),
                root_in_loop: Some(root_in_loop),
                outer_nest: nest,
                entry: per_entry.then_some(site),
            };
            if let Some(c) = build(s, k, at) {
                used += 1 + c.sites;
                admitted.get_or_insert(at);
                out.push((k, c));
            }
        }
        if out.is_empty() {
            return None;
        }
        self.admitted = admitted.unwrap();
        self.inline_sites = used;
        Some(out)
    }
}
