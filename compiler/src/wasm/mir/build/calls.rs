//! calls: methods of `Run`, split out of `build.rs` unchanged.

use super::*;

impl<'s, 'a> Run<'s, 'a> {
    /// Give a generic `call` the site's likely callees (`likely_targets`),
    /// for its lowering's direct arms.
    pub(super) fn attach_targets(&mut self, inst: mir::Inst) {
        if let Some((classes, complete)) = self.store_mask.clone() {
            if matches!(
                self.f.insts[inst].op,
                Opcode::StoreField(_) | Opcode::JsSetProp(..) | Opcode::SetPropData(_)
            ) {
                let a = self.f.attachments.push(mir::func::Attachment {
                    site: Some(self.site(self.pc)),
                    field_types: classes
                        .iter()
                        .map(|&(k, t)| (k, mir::func::encode_tags(t)))
                        .collect(),
                    field_types_complete: complete,
                    ..Default::default()
                });
                self.f.insts[inst].attach = Some(a);
                return;
            }
        }
        if self.ta_poly_site
            && matches!(
                self.f.insts[inst].op,
                Opcode::JsGetElem
                    | Opcode::GetElemData
                    | Opcode::JsSetElem(..)
                    | Opcode::SetElemData(_)
            )
        {
            let a = self.f.attachments.push(mir::func::Attachment {
                site: Some(self.site(self.pc)),
                ta_poly: true,
                ..Default::default()
            });
            self.f.insts[inst].attach = Some(a);
            return;
        }
        if self.f.insts[inst].op != Opcode::Call || self.likely_targets.is_empty() {
            return;
        }
        let targets = std::mem::take(&mut self.likely_targets);
        let a = self.f.attachments.push(mir::func::Attachment {
            site: Some(self.site(self.pc)),
            ic_cell: None,
            call_cell: None,
            slot: None,
            field_types: vec![],
            field_types_complete: false,
            targets,
            ta_poly: false,
        });
        self.f.insts[inst].attach = Some(a);
    }

    /// A terminator that may change what an `Obj` slot proves (a call, a
    /// generic op: it kills layout claims on an edge the build continues
    /// on) weakens each such slot's value to `obj` first, in the current
    /// block (§5's fence rule), and the slot continues as `ObjHint`.
    /// Operands the op popped are renamed the same way (`repush`). Throw
    /// exits need nothing: they only box.
    pub(super) fn fence_params(&mut self, op: &Opcode, args: &[mir::Value], succs: &mut [Edge]) {
        let _ = succs;
        let tys: Vec<MType> = args.iter().map(|&v| self.f.ty(v)).collect();
        let fx = mir::ops::effects(op, &tys, &self.mm);
        if !matches!(
            op.kill_site(&fx),
            mir::ops::KillSite::OkEdge | mir::ops::KillSite::DirtyEdge
        ) {
            return;
        }
        let mut done: Vec<(mir::Value, Slot)> = vec![];
        let olds: Vec<Slot> = self.st.iter().chain(&self.pre).copied().collect();
        for x in olds {
            if !matches!(x.ty, Ty::Obj(..) | Ty::Ctor(..))
                || !fx.kill.matches(&x.ty.mir())
                || done.iter().any(|&(v, _)| v == x.v)
            {
                continue;
            }
            let y = self.demote(x);
            done.push((x.v, y));
        }
        if done.is_empty() {
            return;
        }
        for (v, y) in done {
            for x in self
                .st
                .iter_mut()
                .chain(self.pre.iter_mut())
                .filter(|x| x.v == v)
            {
                *x = y;
            }
            self.renames.push((v, y));
        }
        // An exit at this op's pc made before the fence may be reached
        // after it: the next one is made from the renamed state.
        self.exit_blk = None;
    }

    /// Push `x`, an operand this op popped, back: as its fence renamed it,
    /// if one did (`fence_params`).
    /// `recv.length` as a typed op (§7 `length.{string,array}`), pushing
    /// the int32 it is: a proven string's length word; for a receiver the
    /// builder has evidence is an array (a native object its element
    /// accesses guarded, or an array layout's value class), its elements
    /// header's, guarded to an array and to int32 (a miss exits). False,
    /// with nothing emitted, for anything else (the generic read, whose
    /// lowering tests the same cases at run time).
    pub(super) fn length_op(&mut self, recv: Slot) -> bool {
        if let Ty::Val(t) = recv.ty {
            if t.is_nonempty_subset_of(TagSet::STRING) {
                let s = self.inst(
                    Opcode::Unbox(UnboxKind::Str),
                    vec![recv.v],
                    Some(MType::STR_TOP),
                );
                let n = self.inst(
                    Opcode::LengthString,
                    vec![s],
                    Some(MType::i32_range(0, (1 << 30) - 2)),
                );
                self.push(n, Ty::I32);
                return true;
            }
        }
        let array_cls = |k: u32| self.mm.array_key_min.is_some_and(|min| k + 1 >= min);
        let evidence =
            recv.ty == Ty::Native || self.val_cls.get(&recv.v).is_some_and(|&k| array_cls(k));
        if !evidence || !TagSet::OBJECT.subset_of(recv.ty.tags()) {
            return false;
        }
        let o = match recv.ty {
            Ty::Native | Ty::Obj(..) | Ty::ObjHint(_) | Ty::Ctor(..) => recv.v,
            Ty::Val(_) => self.guard(
                Opcode::GuardUnbox(UnboxKind::Obj),
                vec![recv.v],
                MType::OBJ_TOP,
            ),
            _ => return false,
        };
        let arr = MType::Obj(ObjInfo::kind(ObjKind::Array));
        let o = self.guard(Opcode::GuardKind(ObjKind::Array), vec![o], arr);
        let len = self.inst(
            Opcode::LengthArray,
            vec![o],
            Some(MType::int_range(0, u32::MAX.into())),
        );
        let n = self.guard(
            Opcode::IntToI32,
            vec![len],
            MType::i32_range(0, i64::from(i32::MAX)),
        );
        self.push(n, Ty::I32);
        true
    }

    /// `Math.<fn>(args)` of a function with a typed op (`math_fns`), with
    /// `argc` its arity: the callee checked to be the pristine native, the
    /// arguments to be numbers (exiting at the call on a miss: the site
    /// runs in baseline then), and the op on raw f64s, whose result is a
    /// raw f64 rather than a boxed value to guard. False if not such a
    /// call (nothing emitted).
    pub(super) fn math_call(&mut self, argc: usize) -> bool {
        let n = self.st.len();
        let Some(&m) = self.math_fns.get(&self.st[n - argc - 2].v) else {
            return false;
        };
        if argc != m.arity() {
            return false;
        }
        let operands: Vec<Slot> = self.st.drain(n - argc - 2..).collect();
        let callee = self.boxed(operands[0]);
        let name = format!("Math.{}", math_fn_name(m));
        let found = self
            .mm
            .natives
            .iter()
            .find(|(_, d)| d.name == name)
            .map(|(id, _)| id);
        let nid = match found {
            Some(id) => id,
            None => self.mm.natives.push(mir::module::NativeDef { name }),
        };
        self.guard(
            Opcode::CheckNative(nid),
            vec![callee],
            MType::Fact(mir::types::FactKind::NativeIntact(nid)),
        );
        let mut args = vec![];
        for x in &operands[2..] {
            let f = if x.ty.num().is_some() {
                self.as_f64(*x)
            } else {
                let b = self.boxed(*x);
                self.guard(
                    Opcode::GuardUnbox(UnboxKind::F64Num),
                    vec![b],
                    MType::F64_TOP,
                )
            };
            args.push(f);
        }
        let r = self.inst(Opcode::Math(m), args, Some(MType::F64_TOP));
        self.push(r, Ty::F64);
        true
    }

    /// A generic op with a static kill (`ok`, `err`): the builder attaches
    /// its prediction witness (§4.5), which for a generic op is "may kill
    /// anything".
    pub(super) fn js_static(
        &mut self,
        op: Opcode,
        args: Vec<mir::Value>,
        out: MType,
    ) -> mir::Value {
        let ok = self.new_block();
        let p = self.f.add_param(ok, out);
        let err = self.exit_block(true);
        let mut succs = vec![
            Edge {
                block: ok,
                args: vec![EdgeArg::Out(0)],
            },
            Self::goto(err),
        ];
        self.retain_locals(&op);
        self.fence_params(&op, &args, &mut succs);
        let (inst, _) = self.f.add_inst(self.cur, op, args, &[], succs);
        self.f.witnesses[inst] = Some(mir::func::Witness {
            may_kill: mir::types::KillPattern::ALL,
        });
        self.live = false;
        self.at(ok);
        p
    }

    /// A generic op with no output: both success edges continue; an
    /// exception goes to the op's throw block.
    /// A typed field access (`load_field`/`store_field`) whose `ok_dirty`
    /// edge (its IC arm reported dirt: a SLOTS miss) exits at `next`, the
    /// successor pc, with `post` on the stack (the op has happened). The
    /// clean path then keeps every fact, so the accesses after it fold
    /// their guards (§4.2). Returns the result, if `out`. Without a stack
    /// depth at `next`, both edges continue, as `js` has them.
    pub(super) fn js_dirty_exits(
        &mut self,
        op: Opcode,
        args: Vec<mir::Value>,
        out: Option<MType>,
        next: Pc,
        post: Option<Slot>,
    ) -> Option<mir::Value> {
        debug_assert_eq!(self.next_pc, Some(next));
        let dirty = self.dirty_exit(out, post);
        let Some(dirty) = dirty else {
            // Nowhere to exit to: a fence.
            return match out {
                Some(t) => Some(self.js_fence(op, args, t)),
                None => {
                    self.js_void(op, args);
                    None
                }
            };
        };
        let ok = self.new_block();
        let p = out.map(|t| self.f.add_param(ok, t));
        let clean = Edge {
            block: ok,
            args: p.map(|_| vec![EdgeArg::Out(0)]).unwrap_or_default(),
        };
        let err = self.exit_block(true);
        // Not `term`: the clean edge is no fence.
        self.retain_locals(&op);
        let (inst, _) =
            self.f
                .add_inst(self.cur, op, args, &[], vec![clean, dirty, Self::goto(err)]);
        self.attach_targets(inst);
        self.live = false;
        self.at(ok);
        p
    }

    /// `js_void` for an op that leaves `post` on the stack (a store's
    /// value), keeping facts on its clean edge (`js_keep`).
    pub(super) fn js_void_keep(&mut self, op: Opcode, args: Vec<mir::Value>, post: Slot) {
        if self.js_keep(op, args.clone(), None, Some(post)).is_none() {
            self.js_void(op, args);
        }
    }

    /// A generic op that keeps the frame's proven layouts on its clean
    /// edge (the epoch keep: the lowering takes `ok_clean` when the
    /// stamp epoch did not move across the op's helper, so no class word
    /// was demoted), its dirty edge exiting at the next pc with the op's
    /// result (`out`) or the value it leaves (`post`). Only where there is
    /// something to keep and the stack at the next pc is this op's
    /// result; else `None`, and the caller fences as before.
    pub(super) fn js_keep(
        &mut self,
        op: Opcode,
        args: Vec<mir::Value>,
        out: Option<MType>,
        post: Option<Slot>,
    ) -> Option<Option<mir::Value>> {
        use mir::ops::KillSite;
        // A callee built for inlining keeps with no `Obj` of its own: its
        // caller's facts ride on its every kill exiting.
        if self.s.inline_depth == 0
            && !self
                .st
                .iter()
                .chain(&self.pre)
                .any(|x| matches!(x.ty, Ty::Obj(..)))
        {
            return None;
        }
        let tys: Vec<MType> = args.iter().map(|&v| self.f.ty(v)).collect();
        let fx = mir::ops::effects(&op, &tys, &self.mm);
        if op.kill_site(&fx) != KillSite::DirtyEdge {
            return None;
        }
        let dirty = self.dirty_exit(out, post)?;
        let ok = self.new_block();
        let p = out.map(|t| self.f.add_param(ok, t));
        let clean = Edge {
            block: ok,
            args: p.map(|_| vec![EdgeArg::Out(0)]).unwrap_or_default(),
        };
        let err = self.exit_block(true);
        self.retain_locals(&op);
        let (inst, _) =
            self.f
                .add_inst(self.cur, op, args, &[], vec![clean, dirty, Self::goto(err)]);
        self.attach_targets(inst);
        self.live = false;
        self.at(ok);
        Some(p)
    }

    /// Whether a kill here can exit at the next pc (`dirty_exit`), with
    /// `results` values there on top of the frame as it is now.
    pub(super) fn keepable(&self, results: usize) -> bool {
        self.next_pc
            .and_then(|next| self.s.depths.at(next))
            .is_some_and(|d| d as usize == self.st.len() - self.frame_len() + results)
    }

    /// The dirty edge of a kill that keeps facts on its clean one: an exit
    /// at the next pc with the frame as it is now plus the op's result
    /// (`out`, the edge's `Out(0)`) or the value it leaves (`post`). `Obj`
    /// slots reach it through weaker params, or, for an inline splice
    /// (`weakened`), as weakened copies made here: the callee's dirty
    /// exits name the edge's args in blocks after the kill, where no fact
    /// may be live. `None` where there is no such exit (`keepable`).
    pub(super) fn dirty_exit(&mut self, out: Option<MType>, post: Option<Slot>) -> Option<Edge> {
        self.dirty_exit_as(out, post, false)
    }

    pub(super) fn dirty_exit_as(
        &mut self,
        out: Option<MType>,
        post: Option<Slot>,
        weakened: bool,
    ) -> Option<Edge> {
        if !self.keepable(usize::from(post.is_some() || out.is_some())) {
            return None;
        }
        let next = self.next_pc?;
        let out_ty = match out {
            None => None,
            Some(MType::Bool) => Some(Ty::Bool),
            Some(MType::Val(v)) => Some(Ty::Val(v.tags)),
            Some(_) => return None,
        };
        let olds: Vec<Slot> = self.st.iter().chain(&post).copied().collect();
        let mut copies: Vec<(mir::Value, mir::Value)> = vec![];
        if weakened {
            for x in &olds {
                if matches!(x.ty, Ty::Obj(..) | Ty::Ctor(..))
                    && !copies.iter().any(|&(v, _)| v == x.v)
                {
                    let w = self.weaken(x.v, MType::OBJ_TOP);
                    copies.push((x.v, w));
                }
            }
        }
        let saved = (self.cur, self.live);
        let b = self.new_block();
        let dp = out.map(|t| self.f.add_param(b, t));
        let mut objs: Vec<(mir::Value, mir::Value)> = vec![];
        for x in &olds {
            if matches!(x.ty, Ty::Obj(..) | Ty::Ctor(..)) && !objs.iter().any(|&(v, _)| v == x.v) {
                let p = match copies.iter().find(|&&(v, _)| v == x.v) {
                    Some(&(_, w)) => w,
                    None => self.f.add_param(b, MType::OBJ_TOP),
                };
                objs.push((x.v, p));
            }
        }
        self.at(b);
        let weaker = |x: Slot, objs: &[(mir::Value, mir::Value)]| match (
            x.ty,
            objs.iter().find(|&&(v, _)| v == x.v),
        ) {
            (Ty::Obj(k, ..), Some(&(_, p))) => Slot {
                v: p,
                ty: Ty::ObjHint(k),
            },
            // The block only exits, which boxes it.
            (Ty::Ctor(k, ..), Some(&(_, p))) => Slot {
                v: p,
                ty: Ty::ObjHint(KeyRange::one(crate::ids::LayoutKey::new(k))),
            },
            _ => x,
        };
        let mut st: Vec<Slot> = self.st.iter().map(|&x| weaker(x, &objs)).collect();
        match (dp, post) {
            (Some(v), _) => st.push(Slot {
                v,
                ty: out_ty.unwrap(),
            }),
            (None, Some(x)) => st.push(weaker(x, &objs)),
            (None, None) => {}
        }
        let ops = self.exit_operands(next, &st);
        let eop = self.exit_op(next, false);
        self.term(eop, ops, vec![]);
        (self.cur, self.live) = saved;
        let mut dargs: Vec<EdgeArg> = dp.map(|_| vec![EdgeArg::Out(0)]).unwrap_or_default();
        if !weakened {
            dargs.extend(objs.iter().map(|&(v, _)| EdgeArg::Value(v)));
        }
        Some(Edge {
            block: b,
            args: dargs,
        })
    }

    pub(super) fn js_void(&mut self, op: Opcode, args: Vec<mir::Value>) {
        let ok = self.new_block();
        let err = self.exit_block(true);
        self.term(
            op,
            args,
            vec![Self::goto(ok), Self::goto(ok), Self::goto(err)],
        );
        self.at(ok);
    }

    /// A call whose predicted targets are inlined (§5.5): the callee
    /// guarded to each target's script in turn, each target's MIR spliced
    /// in on its hit; the ordinary call when none matches. Returns the
    /// result, at the continuation. With `this_raw` (an object under
    /// construction, `ctor_pass`, where `this_ctor`; else a published one
    /// of a proven type), the callees receive it as their `this`, built for
    /// it.
    pub(super) fn inline_call_this(
        &mut self,
        targets: &[(ScriptId, std::rc::Rc<super::super::inline::Callee>)],
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
            self.retain_all();
            let frame_this = this_raw.map(|_| vals[1]);
            match super::super::inline::splice(
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
                self.retain_locals(&op);
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
    pub(super) fn inline_construct(
        &mut self,
        k: ScriptId,
        callee: &std::rc::Rc<super::super::inline::Callee>,
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
    pub(super) fn splice_construct(
        &mut self,
        k: ScriptId,
        callee: &std::rc::Rc<super::super::inline::Callee>,
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
        self.retain_all();
        match super::super::inline::splice(
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

    /// `target.apply(this, arguments)` at a proven forward site
    /// (`compute_apply_fwd_pcs`: the arguments object feeds only such calls,
    /// so it was never made), operands `apply, target, this, placeholder`.
    /// With the `.apply` the pristine builtin, the site's known targets
    /// are inlined, their formals read from this frame's actuals; any other
    /// target, or another `.apply`, forwards through the runtime.
    pub(super) fn apply_forward(
        &mut self,
        vals: &[mir::Value],
        this_ctor: Option<((u32, FieldSet, bool), Slot)>,
    ) -> mir::Value {
        let site = self.site(self.pc);
        let facts = &self.s.ctx.facts;
        // Resolved for the site that entered this copy where the analysis
        // has it per entry.
        let per_entry = self
            .s
            .ictx
            .entry
            .and_then(|e| facts.apply_targets_in.get(&(e, site)));
        let sids: Vec<ScriptId> = match per_entry.or_else(|| facts.apply_targets.get(&site)) {
            Some(&k) => vec![k],
            None => facts
                .apply_target_sets
                .get(&site)
                .cloned()
                .unwrap_or_default(),
        };
        let helper = (Opcode::ApplyFwd, vec![vals[0], vals[1], vals[2]]);
        // A target reading its actuals would need this frame's count. A
        // `this` under construction (a wrapper constructor's forward to its
        // `initialize`): targets built for it, as `call_forward`'s.
        let tc = this_ctor.map(|(c, _)| c);
        let build = |s: &Shape<'a>, k, ictx| {
            s.callee_ctx(k, &[], tc, None, ictx)
                .filter(|c| !c.reads_actuals)
        };
        let Some(targets) = self.admit(&sids, false, &build) else {
            return self.js(helper.0, helper.1, MType::VAL_TOP);
        };
        let raw = this_ctor.map(|(_, x)| self.ctor_pass(x, vals[2]));
        // Arms with fences meet at `join`: `Obj` slots go in as `ObjHint`.
        // Without one (both arms keep), they keep their facts.
        if targets.iter().any(|(_, c)| c.fenced) || !self.keepable(1) {
            self.demote_objs();
        }
        let join = self.new_block();
        let result = self.f.add_param(join, MType::VAL_TOP);
        let (fast, slow) = (self.new_block(), self.new_block());
        let is_apply = self.inst(
            Opcode::JsIsBuiltin(crate::wasm::translate::BC_FUN_APPLY),
            vec![vals[0]],
            Some(MType::Bool),
        );
        self.term(
            Opcode::Br,
            vec![is_apply],
            vec![Self::goto(fast), Self::goto(slow)],
        );
        self.at(fast);
        let n = targets
            .iter()
            .map(|(_, c)| c.f.frame.formals)
            .max()
            .unwrap_or(0);
        let mut call = vec![vals[1], vals[2]];
        for k in 0..n {
            call.push(self.inst(Opcode::ActualArgOr(k), vec![], Some(MType::VAL_TOP)));
        }
        let r = self.inline_call_this(&targets, &call, Some(helper.clone()), raw, raw.is_some());
        if raw.is_some() {
            self.ctor_after(&targets, vals[2]);
        }
        self.term(
            Opcode::Jump,
            vec![],
            vec![Edge {
                block: join,
                args: vec![EdgeArg::Value(r)],
            }],
        );
        self.at(slow);
        if raw.is_some() {
            // Another `.apply` for a `this` under construction: baseline's
            // (`inline_call_this`'s rule).
            let ex = self.exit_block(false);
            self.term(Opcode::Jump, vec![], vec![Self::goto(ex)]);
        } else {
            let r = self.js(helper.0, helper.1, MType::VAL_TOP);
            self.term(
                Opcode::Jump,
                vec![],
                vec![Edge {
                    block: join,
                    args: vec![EdgeArg::Value(r)],
                }],
            );
        }
        self.at(join);
        result
    }

    /// `hasOwnProperty.call(o, k)` where the analysis resolved the site's
    /// target to the builtin (`apply_natives`).
    pub(super) fn hasown_site(&self) -> bool {
        let site = self.site(self.pc);
        let facts = &self.s.ctx.facts;
        facts.apply_sites.get(&site) == Some(&crate::facts::CallForm::Call)
            && facts.apply_natives.get(&site) == Some(&crate::facts::ApplyNative::HasOwnProperty)
    }

    /// The native forward, operands `call, target, o, k`: with the `.call`
    /// and the target the pristine builtins, the `HasOwn` op on `k, o`;
    /// otherwise the generic call.
    pub(super) fn hasown_call(&mut self, vals: &[mir::Value]) -> mir::Value {
        use crate::wasm::translate::{BC_FUN_CALL, BC_OBJ_HASOWN};
        self.demote_for_generic_arm(1);
        let join = self.new_block();
        let result = self.f.add_param(join, MType::VAL_TOP);
        let (check, fast, slow) = (self.new_block(), self.new_block(), self.new_block());
        let is_call = self.inst(
            Opcode::JsIsBuiltin(BC_FUN_CALL),
            vec![vals[0]],
            Some(MType::Bool),
        );
        self.term(
            Opcode::Br,
            vec![is_call],
            vec![Self::goto(check), Self::goto(slow)],
        );
        self.at(check);
        let is_hasown = self.inst(
            Opcode::JsIsBuiltin(BC_OBJ_HASOWN),
            vec![vals[1]],
            Some(MType::Bool),
        );
        self.term(
            Opcode::Br,
            vec![is_hasown],
            vec![Self::goto(fast), Self::goto(slow)],
        );
        for (b, r) in [(fast, true), (slow, false)] {
            self.at(b);
            let r = if r {
                self.js(
                    Opcode::JsRt(RtOp::HasOwn),
                    vec![vals[3], vals[2]],
                    MType::val(TagSet::BOOLEAN),
                )
            } else {
                self.js(Opcode::Call, vals.to_vec(), MType::VAL_TOP)
            };
            self.term(
                Opcode::Jump,
                vec![],
                vec![Edge {
                    block: join,
                    args: vec![EdgeArg::Value(r)],
                }],
            );
        }
        self.at(join);
        result
    }

    /// `target.call(thisArg, args…)` at a site whose target the analysis
    /// resolved (`apply_targets`, `apply_target_sets`), operands `call,
    /// target, thisArg, args…`: with the `.call` the pristine builtin, the
    /// targets are inlined with `thisArg` as their `this` (a delegating
    /// constructor's `Super.call(this, …)`), built knowing the closures
    /// `fns` the call passes; any other target, or another `.call`, calls
    /// generically. `None` where nothing can be inlined.
    pub(super) fn call_forward(
        &mut self,
        vals: &[mir::Value],
        fns: &[Option<ScriptId>],
        this_ctor: Option<((u32, FieldSet, bool), Slot, mir::Value)>,
    ) -> Option<mir::Value> {
        let site = self.site(self.pc);
        let facts = &self.s.ctx.facts;
        if facts.apply_sites.get(&site) != Some(&crate::facts::CallForm::Call) {
            return None;
        }
        let sids: Vec<ScriptId> = match facts.apply_targets.get(&site) {
            Some(&k) => vec![k],
            None => facts
                .apply_target_sets
                .get(&site)
                .cloned()
                .unwrap_or_default(),
        };
        let tc = this_ctor.map(|(c, ..)| c);
        let targets = self.admit(&sids, false, &|s, k, ictx| {
            s.callee_ctx(k, fns, tc, None, ictx)
        })?;
        let raw = this_ctor.map(|(_, x, boxed)| self.ctor_pass(x, boxed));
        let generic = (Opcode::Call, vals.to_vec());
        // Arms with fences meet at `join`: `Obj` slots go in as `ObjHint`.
        if targets.iter().any(|(_, c)| c.fenced) || !self.keepable(1) {
            self.demote_objs();
        }
        let join = self.new_block();
        let result = self.f.add_param(join, MType::VAL_TOP);
        let (fast, slow) = (self.new_block(), self.new_block());
        let is_call = self.inst(
            Opcode::JsIsBuiltin(crate::wasm::translate::BC_FUN_CALL),
            vec![vals[0]],
            Some(MType::Bool),
        );
        self.term(
            Opcode::Br,
            vec![is_call],
            vec![Self::goto(fast), Self::goto(slow)],
        );
        self.at(fast);
        let r = self.inline_call_this(
            &targets,
            &vals[1..],
            Some(generic.clone()),
            raw,
            raw.is_some(),
        );
        if raw.is_some() {
            self.ctor_after(&targets, vals[2]);
        }
        self.term(
            Opcode::Jump,
            vec![],
            vec![Edge {
                block: join,
                args: vec![EdgeArg::Value(r)],
            }],
        );
        self.at(slow);
        let r = self.js(generic.0, generic.1, MType::VAL_TOP);
        self.term(
            Opcode::Jump,
            vec![],
            vec![Edge {
                block: join,
                args: vec![EdgeArg::Value(r)],
            }],
        );
        self.at(join);
        Some(result)
    }

    /// A generic op: both success edges continue with its output (of type
    /// `out`); an exception goes to the op's throw block.
    pub(super) fn js(&mut self, op: Opcode, args: Vec<mir::Value>, out: MType) -> mir::Value {
        if let Some(r) = self.js_keep(op, args.clone(), Some(out), None) {
            return r.unwrap();
        }
        self.js_fence(op, args, out)
    }

    /// A generic op whose both success edges continue (a fence).
    pub(super) fn js_fence(&mut self, op: Opcode, args: Vec<mir::Value>, out: MType) -> mir::Value {
        let ok = self.new_block();
        let p = self.f.add_param(ok, out);
        let err = self.exit_block(true);
        let e = Edge {
            block: ok,
            args: vec![EdgeArg::Out(0)],
        };
        self.term(op, args, vec![e.clone(), e, Self::goto(err)]);
        self.at(ok);
        p
    }
}
