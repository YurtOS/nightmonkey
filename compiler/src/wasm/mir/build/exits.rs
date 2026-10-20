//! exits: methods of `Run`, split out of `build.rs` unchanged.

use super::*;

impl<'s, 'a> Run<'s, 'a> {
    /// `x`, as `ObjHint` if it is an `Obj`; boxed if it is a `Ctor` (an
    /// object under construction that a fence may have reached: its state
    /// is unknown, and a published-layout hint would be wrong).
    pub(super) fn demote(&mut self, x: Slot) -> Slot {
        match x.ty {
            Ty::Obj(k, ..) => Slot {
                v: self.weaken(x.v, MType::OBJ_TOP),
                ty: Ty::ObjHint(k),
            },
            Ty::Ctor(k, n, t) => {
                let v = self.boxed(x);
                self.ctor_hint.insert(v, (k, n, t));
                Slot {
                    v,
                    ty: Ty::Val(TagSet::OBJECT),
                }
            }
            _ => x,
        }
    }

    /// Every `Obj` slot, as `ObjHint`: in the state and in this op's
    /// pre-state (which its exits are made from; exits made before are
    /// dropped, an inlined callee's reaching its caller's through
    /// `exit.inline`), for `repush` too.
    pub(super) fn demote_objs(&mut self) {
        let mut done: Vec<(mir::Value, Slot)> = vec![];
        let olds: Vec<Slot> = self.st.iter().chain(&self.pre).copied().collect();
        for x in olds {
            if !matches!(x.ty, Ty::Obj(..) | Ty::Ctor(..)) || done.iter().any(|&(v, _)| v == x.v) {
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
        self.exit_blk = None;
        self.throw_blk = None;
    }

    /// Before a diamond one of whose arms is a generic op (`js`), with
    /// `results` values on top at the next pc: the slots that op's fence
    /// would demote on its own arm, demoted here for every arm, so the arms
    /// meet with one state (a fence on one arm alone renames its slots
    /// there, and a use after the join is then not dominated by the new
    /// name). Where the arm cannot keep (no dirty exit), every `Obj` and
    /// `Ctor` slot; where it can, `js_keep` still declines, and so fences,
    /// with no `Obj` slot live at the top level, demoting the `Ctor` slots.
    pub(super) fn demote_for_generic_arm(&mut self, results: usize) {
        if !self.keepable(results) {
            self.demote_objs();
        } else if self.s.inline_depth == 0
            && !self
                .st
                .iter()
                .chain(&self.pre)
                .any(|x| matches!(x.ty, Ty::Obj(..)))
        {
            self.demote_ctors();
        }
    }

    /// As `demote_for_generic_arm`, for an op that pops `v` and pushes it
    /// back (a store): `v` too, as it is demoted.
    pub(super) fn demote_value_for_generic_arm(&mut self, v: Slot) -> Slot {
        let objs = self
            .st
            .iter()
            .chain(&self.pre)
            .any(|x| matches!(x.ty, Ty::Obj(..)));
        if !self.keepable(1) {
            self.demote_objs();
            self.demote(v)
        } else if self.s.inline_depth == 0 && !objs {
            self.demote_ctors();
            if matches!(v.ty, Ty::Ctor(..)) {
                self.demote(v)
            } else {
                v
            }
        } else {
            v
        }
    }

    /// Every `Ctor` slot (an object under construction), as a boxed
    /// object, in the state and in this op's pre-state
    /// (`demote_for_generic_arm`).
    pub(super) fn demote_ctors(&mut self) {
        let mut done: Vec<(mir::Value, Slot)> = vec![];
        let olds: Vec<Slot> = self.st.iter().chain(&self.pre).copied().collect();
        for x in olds {
            if !matches!(x.ty, Ty::Ctor(..)) || done.iter().any(|&(v, _)| v == x.v) {
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
        self.exit_blk = None;
        self.throw_blk = None;
    }

    /// The frame `st` as exit operands at `pc`, all `Val(⊤)`, recording the
    /// stack depth there.
    pub(super) fn exit_operands(&mut self, pc: Pc, st: &[Slot]) -> Vec<mir::Value> {
        // Baseline has no elided arguments object: an exit whose state
        // holds the placeholder makes the real one first (into the frame
        // slots that hold it, and in the operands), at the exit's own cost.
        let mut st = st.to_vec();
        let ph = self.args_placeholder;
        let holds = |i: usize, x: &Slot| {
            Some(x.v) == ph || self.args_locals.contains(&i) || self.args_copies.contains(&x.v)
        };
        if ph.is_some() && st.iter().enumerate().any(|(i, x)| holds(i, x)) {
            let ok = self.new_block();
            let obj = self.f.add_param(ok, MType::val(TagSet::OBJECT));
            let err = self.new_block();
            // Retained as the exit has them (a dirty exit's are weakened).
            let live = std::mem::replace(&mut self.st, st.clone());
            self.retain_locals(&Opcode::ArgsObject);
            self.st = live;
            let (t, _) = self.f.add_inst(
                self.cur,
                Opcode::ArgsObject,
                vec![],
                &[],
                vec![
                    Edge {
                        block: ok,
                        args: vec![EdgeArg::Out(0)],
                    },
                    Self::goto(err),
                ],
            );
            self.f.witnesses[t] = Some(mir::func::Witness {
                may_kill: mir::types::KillPattern::ALL,
            });
            // Out of memory making it: throw with the state as it is.
            self.at(err);
            let saved = self.args_placeholder.take();
            let ops = self.exit_operands(pc, &st);
            let eop = self.exit_op(pc, true);
            self.term(eop, ops, vec![]);
            self.args_placeholder = saved;
            self.at(ok);
            for (i, x) in st.iter_mut().enumerate() {
                if Some(x.v) == ph
                    || self.args_locals.contains(&i)
                    || self.args_copies.contains(&x.v)
                {
                    if self.write_through(i) {
                        self.inst(
                            Opcode::FrameStore(u32::try_from(i).unwrap()),
                            vec![obj],
                            None,
                        );
                    }
                    *x = Slot {
                        v: obj,
                        ty: Ty::Val(TagSet::OBJECT),
                    };
                }
            }
        }
        let st = &st[..];
        let depth = st.len() - self.frame_len();
        self.f
            .frame
            .depths
            .insert(pc, u32::try_from(depth).unwrap());
        // A slot dead at the exit's pc goes as `const.val dead`: baseline
        // never reads it before writing it, so the frame keeps whatever
        // (valid) value it has. A dead local overwritten after an earlier
        // retaining store had its new value retained too (`retain_dead_overwrite`),
        // so the frame never holds an object the local no longer does.
        let live = self.s.live.get(&pc).cloned();
        let mut dead = None;
        let mut ops = vec![];
        for (i, &x) in st.iter().enumerate() {
            // A written-through slot is already in the frame.
            let in_frame = self.write_through(i);
            let is_dead =
                in_frame || x.ty == Ty::Dead || live.as_ref().is_some_and(|l| i < l.len() && !l[i]);
            if is_dead && !in_frame && self.overwritten.contains(&i) {
                // The frame may hold an object this local no longer does:
                // the exit overwrites it (baseline would, before any read).
                ops.push(self.const_val(ConstVal::Undefined));
            } else if is_dead {
                let d = *dead.get_or_insert_with(|| self.const_val(ConstVal::Dead));
                ops.push(d);
            } else {
                // As it is: the lowering boxes it at the exit hub.
                ops.push(x.v);
            }
        }
        ops
    }

    pub(super) fn exit_op(&self, pc: Pc, throw: bool) -> Opcode {
        let (nargs, nlocals) = (self.s.nargs, self.s.nlocals);
        if throw {
            Opcode::ExitThrow { pc, nargs, nlocals }
        } else {
            Opcode::Exit { pc, nargs, nlocals }
        }
    }

    /// The op's shared exit (or throw) block: the state before the op.
    pub(super) fn exit_block(&mut self, throw: bool) -> mir::Block {
        let slot = if throw { self.throw_blk } else { self.exit_blk };
        if let Some(b) = slot {
            return b;
        }
        let saved = (self.cur, self.live);
        let b = self.new_block();
        self.at(b);
        let pre = self.pre.clone();
        let ops = if throw && self.s.throws_uncaught() {
            // Baseline's landing for it only returns the error: no state.
            self.f.frame.depths.insert(
                self.pc,
                u32::try_from(pre.len() - self.frame_len()).unwrap(),
            );
            let d = self.const_val(ConstVal::Dead);
            vec![d; pre.len()]
        } else {
            self.exit_operands(self.pc, &pre)
        };
        let op = self.exit_op(self.pc, throw);
        self.term(op, ops, vec![]);
        (self.cur, self.live) = saved;
        if throw {
            self.throw_blk = Some(b);
        } else {
            self.exit_blk = Some(b);
        }
        b
    }
}
