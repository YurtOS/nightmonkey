//! emit: methods of `Run`, split out of `calls.rs` unchanged.

use super::*;

impl<'s, 'a> Run<'s, 'a> {
    /// A terminator that may change what an `Obj` slot proves (a call, a
    /// generic op: it kills layout claims on an edge the build continues
    /// on) weakens each such slot's value to `obj` first, in the current
    /// block (§5's fence rule), and the slot continues as `ObjHint`.
    /// Operands the op popped are renamed the same way (`repush`). Throw
    /// exits need nothing: they only box.
    pub(in crate::wasm::mir::build) fn fence_params(
        &mut self,
        op: &Opcode,
        args: &[mir::Value],
        succs: &mut [Edge],
    ) {
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

    /// A generic op with a static kill (`ok`, `err`): the builder attaches
    /// its prediction witness (§4.5), which for a generic op is "may kill
    /// anything".
    pub(in crate::wasm::mir::build) fn js_static(
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
    pub(in crate::wasm::mir::build) fn js_dirty_exits(
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
    pub(in crate::wasm::mir::build) fn js_void_keep(
        &mut self,
        op: Opcode,
        args: Vec<mir::Value>,
        post: Slot,
    ) {
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
    pub(in crate::wasm::mir::build) fn js_keep(
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
    pub(in crate::wasm::mir::build) fn keepable(&self, results: usize) -> bool {
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
    pub(in crate::wasm::mir::build) fn dirty_exit(
        &mut self,
        out: Option<MType>,
        post: Option<Slot>,
    ) -> Option<Edge> {
        self.dirty_exit_as(out, post, false)
    }

    pub(in crate::wasm::mir::build) fn dirty_exit_as(
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

    pub(in crate::wasm::mir::build) fn js_void(&mut self, op: Opcode, args: Vec<mir::Value>) {
        let ok = self.new_block();
        let err = self.exit_block(true);
        self.term(
            op,
            args,
            vec![Self::goto(ok), Self::goto(ok), Self::goto(err)],
        );
        self.at(ok);
    }

    /// A generic op: both success edges continue with its output (of type
    /// `out`); an exception goes to the op's throw block.
    pub(in crate::wasm::mir::build) fn js(
        &mut self,
        op: Opcode,
        args: Vec<mir::Value>,
        out: MType,
    ) -> mir::Value {
        if let Some(r) = self.js_keep(op, args.clone(), Some(out), None) {
            return r.unwrap();
        }
        self.js_fence(op, args, out)
    }

    /// A generic op whose both success edges continue (a fence).
    pub(in crate::wasm::mir::build) fn js_fence(
        &mut self,
        op: Opcode,
        args: Vec<mir::Value>,
        out: MType,
    ) -> mir::Value {
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
