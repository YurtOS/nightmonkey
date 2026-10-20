//! guards: methods of `Run`, split out of `build.rs` unchanged.

use super::*;

impl<'s, 'a> Run<'s, 'a> {
    /// The inlining admission (with its per-target rules): which of `sids` the site at `self.pc` inlines,
    /// each built by `build` where the site puts it, in order; `None` for
    /// none. Counts the site, and the sites its callees splice, against
    /// this function's budget.
    pub(super) fn admit(
        &mut self,
        sids: &[ScriptId],
        construct: bool,
        build: &dyn Fn(
            &Shape<'a>,
            ScriptId,
            InlineCtx,
        ) -> Option<std::rc::Rc<super::super::inline::Callee>>,
    ) -> Option<Vec<(ScriptId, std::rc::Rc<super::super::inline::Callee>)>> {
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

    /// Whether an element store at `pc` of `v` owes the array stamp's
    /// RANGES and TYPES claims a clear: the
    /// site's claim, or the intersection of every claim for a receiver
    /// the analysis did not place, unless `v` is proven an int32 inside it
    /// (an element claim is an int32 range). No claim, no duty.
    pub(super) fn ranges_duty(&self, pc: Pc, v: Slot) -> bool {
        let ctx = self.s.ctx;
        let claim = ctx
            .array_elem_in
            .get(&self.site(pc))
            .map(|a| a.range)
            .or(ctx.array_any_claim);
        let Some(r) = claim else { return false };
        let inside = |lo: i64, hi: i64| lo >= r.lo && hi <= r.hi;
        let proven = match self.f.ty(v.v) {
            MType::I32(ir) | MType::Int(ir) => inside(ir.lo, ir.hi),
            MType::Val(s) if s.tags.is_nonempty_subset_of(TagSet::INT32) => {
                s.num.range.is_some_and(|x| inside(x.lo, x.hi))
            }
            _ => false,
        };
        !proven
    }

    /// Guard-at-defs (§8) for the result of the op just built, on top of
    /// the stack: guard it to the analysis's prediction `claim`. The op has
    /// happened, so a failure exits at `next`, the successor pc, with the
    /// result unguarded on the stack (§5.1's resume rule).
    pub(super) fn guard_result(&mut self, claim: crate::facts::Claim, next: Pc, int_first: bool) {
        let prims = claim.prims();
        if claim.is_none() || claim.is_object() || prims.is_empty() {
            return;
        }
        let (op, ty) = match num_claim_ty(claim, int_first) {
            Some(Ty::I32) => (Opcode::GuardUnbox(UnboxKind::I32), Ty::I32),
            Some(Ty::F64) => (Opcode::GuardUnbox(UnboxKind::F64Num), Ty::F64),
            _ => return,
        };
        let x = self.top();
        let Ty::Val(tags) = x.ty else {
            return;
        };
        // A value already proven to have the tags the guard would test (a
        // typed field read under TYPES): the unbox alone, which cannot fail.
        let Opcode::GuardUnbox(kind) = op else {
            unreachable!()
        };
        if tags.is_nonempty_subset_of(kind.tags()) {
            let p = self.inst(Opcode::Unbox(kind), vec![x.v], Some(ty.mir()));
            self.st.pop();
            self.push(p, ty);
            return;
        }
        let depth = self.st.len() - self.frame_len();
        if self.s.depths.at(next) != Some(u32::try_from(depth).unwrap()) {
            return;
        }
        let st = self.st.clone();
        let saved = (self.cur, self.live);
        let fail = self.new_block();
        self.at(fail);
        let ops = self.exit_operands(next, &st);
        let eop = self.exit_op(next, false);
        self.term(eop, ops, vec![]);
        (self.cur, self.live) = saved;
        let ok = self.new_block();
        let p = self.f.add_param(ok, ty.mir());
        self.term(
            op,
            vec![x.v],
            vec![
                Edge {
                    block: ok,
                    args: vec![EdgeArg::Out(0)],
                },
                Self::goto(fail),
            ],
        );
        self.at(ok);
        self.st.pop();
        self.push(p, ty);
    }

    /// Replace every copy of `old` in the state with `new` (the receiver
    /// of an `init_field`, advanced; published). For an object under
    /// construction, every slot of its layout's constructing type is it:
    /// a builder run's only such object is its own `this` (another's, an
    /// inlined `new`'s, lives in that callee's run, and leaves it boxed),
    /// though a join may have given it a second value (an operand copy
    /// made a block param), whose claim the add has made stale.
    pub(super) fn replace_slot(&mut self, old: mir::Value, new: Slot) {
        let key = match new.ty {
            Ty::Ctor(k, ..) => Some(k),
            Ty::Obj(keys, ..) if keys.lo == keys.hi => Some(keys.lo.get()),
            _ => None,
        };
        let mut olds = vec![old];
        for x in self.st.iter_mut() {
            let same = x.v == old || matches!(x.ty, Ty::Ctor(k, ..) if Some(k) == key);
            if same && x.v != new.v {
                if !olds.contains(&x.v) {
                    olds.push(x.v);
                }
                *x = new;
            }
        }
        for o in olds {
            self.renames.push((o, new));
        }
    }

    /// A guard whose failure cannot happen (it restates what the op before
    /// it made so): its fail edge is unreachable.
    pub(super) fn assert_guard(
        &mut self,
        op: Opcode,
        args: Vec<mir::Value>,
        out: MType,
    ) -> mir::Value {
        let ok = self.new_block();
        let p = self.f.add_param(ok, out);
        let never = self.new_block();
        self.term(
            op,
            args,
            vec![
                Edge {
                    block: ok,
                    args: vec![EdgeArg::Out(0)],
                },
                Self::goto(never),
            ],
        );
        self.at(never);
        self.term(Opcode::Unreachable, vec![], vec![]);
        self.at(ok);
        p
    }

    pub(super) fn guard(&mut self, op: Opcode, args: Vec<mir::Value>, out: MType) -> mir::Value {
        let ok = self.new_block();
        let p = self.f.add_param(ok, out);
        let fail = self.exit_block(false);
        self.term(
            op,
            args,
            vec![
                Edge {
                    block: ok,
                    args: vec![EdgeArg::Out(0)],
                },
                Self::goto(fail),
            ],
        );
        self.at(ok);
        p
    }

    /// A method's `this`, guarded once to the layouts the analysis
    /// predicts for it (`this_layouts`), exiting here (nothing has
    /// happened yet) on a miss. Its field accesses then fold their own
    /// guards into this one while no fence intervenes (§10.1), and their
    /// IC fallbacks go with them.
    pub(super) fn guard_this_layout(&mut self) {
        let ctx = self.s.ctx;
        let sid = self.s.sid;
        // A constructor compiled standalone (§2.3; not inlined at a `new`):
        // its `this` is hinted as `create_this` makes it, under
        // construction for the constructor's own layout with no field yet,
        // so its first add guards that state (`guard.ctor K, 0`, exiting on
        // a miss: a call without `new`, an object made elsewhere) and its
        // adds are `init_field`s, as in an inlined construct.
        if matches!(self.top().ty, Ty::Val(_)) {
            if let Some(si) = ctx.stamp_ctors_in.get(&sid) {
                let key = si.layout_id;
                if self.ctor_layout(key).is_some() {
                    let t = self.s.types_bit(key) != 0;
                    let v = self.top().v;
                    self.ctor_hint.insert(v, (key, FieldSet::EMPTY, t));
                }
            }
        }
        let Some(&(lo, hi)) = ctx.facts.this_layouts.get(&sid) else {
            return;
        };
        // A constructor's `this` is still being built: known so where the
        // inlining `new` passed it (`Ctor`), else left unguarded; so is a
        // method a constructor calls on it (`ctor_publish`: early
        // publication stamps it only once every field is there), whose
        // accesses guard it each, reading through the IC on a miss.
        if matches!(self.top().ty, Ty::Ctor(..))
            || ctx.stamp_ctors_in.contains_key(&sid)
            || ctx.deleg_restamps_in.contains_key(&sid)
            || ctx.facts.ctor_publish.contains_key(&sid)
            || ctx.this_layouts_in.get(&sid).is_some_and(|l| l.init_home)
        {
            return;
        }
        let x = self.top();
        let keys = KeyRange { lo, hi };
        // Identity and TYPES are what OPT means (§4.3): where the layouts
        // type their fields, the guard proves TYPES too, and `this`'s
        // field reads are of their predicted types unchecked.
        let types = (lo.get()..=hi.get()).all(|k| self.s.types_bit(k) != 0);
        // A `this` the inlining call passes proven so already.
        if let Ty::Obj(k2, t2, true) = x.ty {
            if keys.contains(&k2) && (t2 || !types) {
                return;
            }
        }
        let v = self.boxed(x);
        let o = self.guard(Opcode::GuardUnbox(UnboxKind::Obj), vec![v], MType::OBJ_TOP);
        // SLOTS too: `this`'s accesses have slot predictions.
        let ty = Ty::Obj(keys, types, true);
        let g = self.guard(
            Opcode::GuardLayout {
                keys,
                types,
                slots: true,
                closed: false,
            },
            vec![o],
            ty.mir(),
        );
        self.st.pop();
        self.push(g, ty);
    }

    /// Boxed `v` as narrow object type `t` (`Native`, `Ta`): unboxed, then
    /// its kind guarded, branching to `fail` on a miss.
    pub(super) fn guard_narrow(&mut self, v: mir::Value, t: Ty, fail: mir::Block) -> mir::Value {
        let kind = match t {
            Ty::Native => ObjKind::Native,
            Ty::Ta(k) => ObjKind::TypedArray(k),
            t => unreachable!("guard_narrow to {t:?}"),
        };
        let ok = self.new_block();
        let u = self.f.add_param(ok, MType::OBJ_TOP);
        self.term(
            Opcode::GuardUnbox(UnboxKind::Obj),
            vec![v],
            vec![
                Edge {
                    block: ok,
                    args: vec![EdgeArg::Out(0)],
                },
                Self::goto(fail),
            ],
        );
        self.at(ok);
        let ok = self.new_block();
        let o = self.f.add_param(ok, t.mir());
        self.term(
            Opcode::GuardKind(kind),
            vec![u],
            vec![
                Edge {
                    block: ok,
                    args: vec![EdgeArg::Out(0)],
                },
                Self::goto(fail),
            ],
        );
        self.at(ok);
        o
    }

    /// Guard boxed receiver `x` to an object of the site's layouts (§4.3):
    /// an object tag test, then the stamp compare, branching to `fallback`
    /// (no params) when either fails.
    pub(super) fn guard_layout_or(
        &mut self,
        x: mir::Value,
        site: &TypedSite,
        fallback: mir::Block,
    ) -> mir::Value {
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
                Self::goto(fallback),
            ],
        );
        self.at(ob);
        let keys = KeyRange {
            lo: crate::ids::LayoutKey::new(site.lo),
            hi: crate::ids::LayoutKey::new(site.hi),
        };
        let t = MType::Obj(ObjInfo {
            layout: Some(LayoutClaim {
                keys,
                types: site.types,
                slots: site.slotted,
                closed: false,
                state: LayoutState::Published,
            }),
            ..ObjInfo::TOP
        });
        let lb = self.new_block();
        let lo = self.f.add_param(lb, t);
        self.term(
            Opcode::GuardLayout {
                keys,
                types: site.types,
                slots: site.slotted,
                closed: false,
            },
            vec![o],
            vec![
                Edge {
                    block: lb,
                    args: vec![EdgeArg::Out(0)],
                },
                Self::goto(fallback),
            ],
        );
        self.at(lb);
        lo
    }

    /// Narrowing from a dynamic test (§4.4): a test of whether `x`'s tag
    /// is in `tags` (negated if `negate`) that the next op branches on,
    /// fused with that branch into one `guard.tags` whose ok edge carries
    /// `x` narrowed to the branch it takes, every slot holding it
    /// replaced there, and whose fail edge is the other branch. False
    /// (nothing emitted) unless the next op is a `JumpIfTrue` or
    /// `JumpIfFalse` that no other edge reaches and `x` is a boxed value
    /// the test can narrow.
    pub(super) fn fuse_test(&mut self, x: Slot, tags: TagSet, negate: bool) -> bool {
        let Ty::Val(have) = x.ty else { return false };
        let narrowed = have.intersect(tags);
        if narrowed.is_empty() || narrowed == have {
            return false;
        }
        let Some(bpc) = self.next_pc else {
            return false;
        };
        if self.s.leaders.contains(&bpc) {
            return false;
        }
        let Some(bop) = self.s.op_at(bpc) else {
            return false;
        };
        if !matches!(bop, JSOp::JumpIfTrue | JSOp::JumpIfFalse) {
            return false;
        }
        let off = self.s.imms(bpc).next_int32().unwrap();
        let (taken, fall) = (bpc.branch(off), bpc + bop.len());
        // Where control goes when the tag is in `tags`, and when not.
        let cond_true = if bop == JSOp::JumpIfTrue { taken } else { fall };
        let cond_false = if bop == JSOp::JumpIfTrue { fall } else { taken };
        let (inside, outside) = if negate {
            (cond_false, cond_true)
        } else {
            (cond_true, cond_false)
        };
        // Narrow the side that learns something: a value that is not null
        // or undefined, rather than one that is.
        let nullish = TagSet::prims(PRIM_NULL | PRIM_UNDEFINED);
        let (tags, narrowed, inside, outside) = if tags.subset_of(nullish) {
            let rest = have.minus(tags);
            (rest, rest, outside, inside)
        } else {
            (tags, narrowed, inside, outside)
        };
        let v = self.boxed(x);
        let (nb, fb) = (self.new_block(), self.new_block());
        let nv = self.f.add_param(nb, MType::val(narrowed));
        self.term(
            Opcode::GuardTags(tags),
            vec![v],
            vec![
                Edge {
                    block: nb,
                    args: vec![EdgeArg::Out(0)],
                },
                Self::goto(fb),
            ],
        );
        self.at(nb);
        let saved = self.st.clone();
        for y in self.st.iter_mut().filter(|y| y.v == x.v) {
            *y = Slot {
                v: nv,
                ty: Ty::Val(narrowed),
            };
        }
        self.jump_to(inside, Some(bpc));
        self.st = saved;
        self.at(fb);
        self.jump_to(outside, Some(bpc));
        true
    }

    /// Narrowing from `x instanceof C` (§4.4), whose boolean is on top of
    /// the stack, where the next op branches on it: the true side knows
    /// `x` is an object (an ordinary function's `instanceof` walks a
    /// prototype chain, which a primitive has none of), so there `x` is
    /// `guard.tags object`ed and replaced in every slot. A primitive can
    /// answer true only through a custom `Symbol.hasInstance`; that exits
    /// at the branch with the result on the stack (the op has happened).
    /// Nothing is emitted unless the next op is a `JumpIfTrue` or
    /// `JumpIfFalse` that no other edge reaches and `x` is a boxed value
    /// that may be an object and may be something else.
    pub(super) fn narrow_instanceof(&mut self, x: Slot) {
        let Ty::Val(have) = x.ty else { return };
        let obj = TagSet::OBJECT;
        if have.intersect(obj).is_empty() || have.subset_of(obj) {
            return;
        }
        let Some(bpc) = self.next_pc else { return };
        if self.s.leaders.contains(&bpc) {
            return;
        }
        let Some(bop) = self.s.op_at(bpc) else { return };
        if !matches!(bop, JSOp::JumpIfTrue | JSOp::JumpIfFalse) {
            return;
        }
        let depth = self.st.len() - self.frame_len();
        if self.s.depths.at(bpc) != Some(u32::try_from(depth).unwrap()) {
            return;
        }
        let off = self.s.imms(bpc).next_int32().unwrap();
        let (taken, fall) = (bpc.branch(off), bpc + bop.len());
        let (cond_true, cond_false) = if bop == JSOp::JumpIfTrue {
            (taken, fall)
        } else {
            (fall, taken)
        };
        let st = self.st.clone();
        let saved = (self.cur, self.live);
        let fail = self.new_block();
        self.at(fail);
        let ops = self.exit_operands(bpc, &st);
        let eop = self.exit_op(bpc, false);
        self.term(eop, ops, vec![]);
        (self.cur, self.live) = saved;
        let r = self.pop();
        let c = self.truthy(r);
        let (tb, fb) = (self.new_block(), self.new_block());
        self.term(Opcode::Br, vec![c], vec![Self::goto(tb), Self::goto(fb)]);
        self.at(tb);
        let nb = self.new_block();
        let nv = self.f.add_param(nb, MType::val(obj));
        self.term(
            Opcode::GuardTags(obj),
            vec![x.v],
            vec![
                Edge {
                    block: nb,
                    args: vec![EdgeArg::Out(0)],
                },
                Self::goto(fail),
            ],
        );
        self.at(nb);
        let saved = self.st.clone();
        for y in self.st.iter_mut().filter(|y| y.v == x.v) {
            *y = Slot {
                v: nv,
                ty: Ty::Val(obj),
            };
        }
        self.jump_to(cond_true, Some(bpc));
        self.st = saved;
        self.at(fb);
        self.jump_to(cond_false, Some(bpc));
    }

    /// Whether boxed `x`'s tag is in `tags`, as a raw bool: a `guard.tags`
    /// whose failure edge merges back rather than exiting.
    pub(super) fn tag_test(&mut self, x: mir::Value, tags: TagSet) -> mir::Value {
        let (t, e, j) = (self.new_block(), self.new_block(), self.new_block());
        let p = self.f.add_param(j, MType::Bool);
        self.f.add_param(t, MType::val(tags));
        self.term(
            Opcode::GuardTags(tags),
            vec![x],
            vec![
                Edge {
                    block: t,
                    args: vec![EdgeArg::Out(0)],
                },
                Self::goto(e),
            ],
        );
        for (b, val) in [(t, true), (e, false)] {
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

    /// Guard root params `params` (all `Val(⊤)`, the frame) up to slot
    /// types `tys`, exiting to `fail` on a miss: one value per slot that is
    /// not dead, with `cur` at the block past the last guard.
    pub(super) fn guard_to(
        &mut self,
        params: &[mir::Value],
        tys: &[Ty],
        fail: mir::Block,
    ) -> Vec<mir::Value> {
        let mut args = vec![];
        for (&v, &t) in params.iter().zip(tys) {
            let op = match t {
                Ty::Dead => continue,
                Ty::I32 => Some(Opcode::GuardUnbox(UnboxKind::I32)),
                Ty::F64 => Some(Opcode::GuardUnbox(UnboxKind::F64Num)),
                Ty::Bool => Some(Opcode::GuardUnbox(UnboxKind::Bool)),
                Ty::Val(tags) if tags == TagSet::ALL => None,
                Ty::Val(tags) => Some(Opcode::GuardTags(tags)),
                Ty::ObjHint(_) => Some(Opcode::GuardUnbox(UnboxKind::Obj)),
                Ty::Fn(k) => {
                    // An object, a function of script `k`; passed boxed.
                    let obj = TagSet::OBJECT;
                    let (b1, b2, b3) = (self.new_block(), self.new_block(), self.new_block());
                    let bv = self.f.add_param(b1, MType::val(obj));
                    self.term(
                        Opcode::GuardTags(obj),
                        vec![v],
                        vec![
                            Edge {
                                block: b1,
                                args: vec![EdgeArg::Out(0)],
                            },
                            Self::goto(fail),
                        ],
                    );
                    self.at(b1);
                    let o = self.f.add_param(b2, MType::OBJ_TOP);
                    self.term(
                        Opcode::GuardUnbox(UnboxKind::Obj),
                        vec![bv],
                        vec![
                            Edge {
                                block: b2,
                                args: vec![EdgeArg::Out(0)],
                            },
                            Self::goto(fail),
                        ],
                    );
                    self.at(b2);
                    self.f
                        .add_param(b3, MType::Obj(ObjInfo::kind(ObjKind::Function(Some(k)))));
                    // The guard compares against the script's address.
                    if let crate::source::SourceObject::Script(ks) = self
                        .s
                        .ctx
                        .source
                        .object(crate::source::SourceObjectId::new(k.get()))
                    {
                        self.mm.script_addrs.insert(k, ks.addr);
                    }
                    self.term(
                        Opcode::GuardScript(k),
                        vec![o],
                        vec![
                            Edge {
                                block: b3,
                                args: vec![EdgeArg::Out(0)],
                            },
                            Self::goto(fail),
                        ],
                    );
                    self.at(b3);
                    args.push(bv);
                    continue;
                }
                Ty::Native | Ty::Ta(_) => {
                    let o = self.guard_narrow(v, t, fail);
                    args.push(o);
                    continue;
                }
                Ty::Obj(keys, types, slots) => {
                    // Unbox, then the layout.
                    let ok = self.new_block();
                    let o = self.f.add_param(ok, MType::OBJ_TOP);
                    self.term(
                        Opcode::GuardUnbox(UnboxKind::Obj),
                        vec![v],
                        vec![
                            Edge {
                                block: ok,
                                args: vec![EdgeArg::Out(0)],
                            },
                            Self::goto(fail),
                        ],
                    );
                    self.at(ok);
                    let ok = self.new_block();
                    let out = self.f.add_param(ok, t.mir());
                    self.term(
                        Opcode::GuardLayout {
                            keys,
                            types,
                            slots,
                            closed: false,
                        },
                        vec![o],
                        vec![
                            Edge {
                                block: ok,
                                args: vec![EdgeArg::Out(0)],
                            },
                            Self::goto(fail),
                        ],
                    );
                    self.at(ok);
                    args.push(out);
                    continue;
                }
                Ty::Ctor(key, n, types) => {
                    // Unbox, then the construction state (§2.3).
                    let ok = self.new_block();
                    let o = self.f.add_param(ok, MType::OBJ_TOP);
                    self.term(
                        Opcode::GuardUnbox(UnboxKind::Obj),
                        vec![v],
                        vec![
                            Edge {
                                block: ok,
                                args: vec![EdgeArg::Out(0)],
                            },
                            Self::goto(fail),
                        ],
                    );
                    self.at(ok);
                    let ok = self.new_block();
                    let out = self.f.add_param(ok, t.mir());
                    self.term(
                        Opcode::GuardCtor {
                            key: crate::ids::LayoutKey::new(key),
                            n,
                            types,
                        },
                        vec![o],
                        vec![
                            Edge {
                                block: ok,
                                args: vec![EdgeArg::Out(0)],
                            },
                            Self::goto(fail),
                        ],
                    );
                    self.at(ok);
                    args.push(out);
                    continue;
                }
            };
            let Some(op) = op else {
                args.push(v);
                continue;
            };
            let ok = self.new_block();
            let out = self.f.add_param(ok, t.mir());
            self.term(
                op,
                vec![v],
                vec![
                    Edge {
                        block: ok,
                        args: vec![EdgeArg::Out(0)],
                    },
                    Self::goto(fail),
                ],
            );
            self.at(ok);
            args.push(out);
        }
        args
    }
}
