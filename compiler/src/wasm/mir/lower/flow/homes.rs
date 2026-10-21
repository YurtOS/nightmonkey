//! homes: methods of `Lower`, split out of `flow.rs` unchanged.

use super::*;

impl<'a> Lower<'a> {
    /// Home slots: every managed value live across a may-GC instruction
    /// gets one, shared among values never live at once. In SSA two
    /// values interfere iff one is live at the other's definition, so
    /// coloring in definition order (reverse postorder, which respects
    /// dominance) against the colored values live there is enough. Also
    /// computes `crossed_out`.
    pub(in crate::wasm::mir::lower) fn homes(
        &mut self,
        reach: &BTreeSet<mir::Block>,
        order: &[mir::Block],
    ) {
        let f = self.f;
        let managed = |v: mir::Value| is_managed(&f.values[v].ty);
        // Per block: the managed values live after each may-GC instruction,
        // and the values live after each definition (block params first).
        let mut cand: BTreeSet<mir::Value> = BTreeSet::new();
        let mut crossed_gen: BTreeMap<mir::Block, BTreeSet<mir::Value>> = BTreeMap::new();
        let mut def_live: Vec<(mir::Block, usize, mir::Value, Vec<mir::Value>)> = vec![];
        for &b in reach {
            let mut live: BTreeSet<mir::Value> = BTreeSet::new();
            for s in f.succs(b) {
                if let Some(l) = self.live_in.get(&s) {
                    live.extend(l.iter().copied());
                }
            }
            let insts = &f.blocks[b].insts;
            for (k, &i) in insts.iter().enumerate().rev() {
                let d = &f.insts[i];
                for e in &d.succs {
                    for a in &e.args {
                        if let EdgeArg::Value(v) = a {
                            live.insert(*v);
                        }
                    }
                }
                for &r in &d.results {
                    live.remove(&r);
                }
                if self.may_gc(i) {
                    let hot = !gc_only_off_path(&d.op);
                    let g = crossed_gen.entry(b).or_default();
                    for &v in live.iter().filter(|&&v| managed(v)) {
                        cand.insert(v);
                        if hot {
                            g.insert(v);
                        }
                    }
                }
                for &r in &d.results {
                    if managed(r) {
                        let mut l: Vec<mir::Value> =
                            live.iter().copied().filter(|&v| managed(v)).collect();
                        l.push(r);
                        def_live.push((b, k + 1, r, l));
                    }
                }
                live.extend(d.args.iter().copied());
            }
            for &p in &f.blocks[b].params {
                if managed(p) {
                    let l: Vec<mir::Value> = live.iter().copied().filter(|&v| managed(v)).collect();
                    def_live.push((b, 0, p, l));
                }
            }
        }
        // Color in definition order.
        let pos: BTreeMap<mir::Block, usize> =
            order.iter().enumerate().map(|(i, &b)| (b, i)).collect();
        def_live.sort_by_key(|&(b, k, v, _)| (pos.get(&b).copied().unwrap_or(usize::MAX), k, v));
        let mut n = 0u32;
        for (_, _, v, live) in def_live {
            if !cand.contains(&v) {
                continue;
            }
            let used: BTreeSet<u32> = live
                .iter()
                .filter(|&&u| u != v)
                .filter_map(|u| self.home.get(u).copied())
                .collect();
            let c = (0..).find(|c| !used.contains(c)).unwrap();
            n = n.max(c + 1);
            self.home.insert(v, c);
        }
        self.nslots = n;
        // What may have crossed a may-GC call, forward to a fixpoint.
        let mut changed = true;
        while changed {
            changed = false;
            for &b in order {
                let mut out: BTreeSet<mir::Value> = BTreeSet::new();
                for p in self.preds.get(&b).cloned().unwrap_or_default() {
                    if let Some(c) = self.crossed_out.get(&p) {
                        out.extend(c.iter().copied());
                    }
                }
                if let Some(g) = crossed_gen.get(&b) {
                    out.extend(g.iter().copied());
                }
                if self.crossed_out.get(&b) != Some(&out) {
                    self.crossed_out.insert(b, out);
                    changed = true;
                }
            }
        }
    }

    /// Enter block `b`: decide where each managed live-in is on entry from
    /// the edges already made into it (every one but back edges), fill
    /// those edges' blocks, and set the emission state.
    ///
    /// A value that may cross a may-GC call before a later (back) edge
    /// reaches `b` enters only in its home slot, so a loop stores it once,
    /// before it. Otherwise it enters in a register if every edge has one
    /// (a param, unless every edge has the same value) or if every edge
    /// but a helper's slow path does (which reloads it); else only in its
    /// home slot, stored along the edges that have not stored it.
    pub(in crate::wasm::mir::lower) fn enter_block(&mut self, b: mir::Block) -> R<()> {
        let f = self.f;
        let wb = self.blocks[&b];
        self.cur = wb;
        let pend = self.pending.remove(&b).unwrap_or_default();
        let me = self.rpo_index[&b];
        let later: Vec<mir::Block> = self
            .preds
            .get(&b)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|p| self.rpo_index.get(p).is_some_and(|&i| i >= me))
            .collect();
        let live: Vec<mir::Value> = self.live_in[&b]
            .iter()
            .copied()
            .filter(|&v| is_managed(&self.ty(v)))
            .collect();
        let mut plan = vec![];
        for &v in &live {
            let at = |p: &PendingEdge| p.snap.get(&v).copied().unwrap_or((None, false));
            let all_reg = pend.iter().all(|p| at(p).0.is_some());
            let all_slot = pend.iter().all(|p| at(p).1);
            let crossed_later = later
                .iter()
                .any(|p| self.crossed_out.get(p).is_some_and(|c| c.contains(&v)));
            let hot: Vec<&PendingEdge> = pend.iter().filter(|p| !p.cold).collect();
            let loc = if crossed_later && self.home.contains_key(&v) {
                Loc::Slot
            } else if all_reg && !pend.is_empty() {
                let first = at(&pend[0]).0;
                if later.is_empty() && pend.iter().all(|p| at(p).0 == first) {
                    Loc::Direct(first.unwrap())
                } else {
                    Loc::Param
                }
            } else if !hot.is_empty() && hot.iter().all(|p| at(p).0.is_some()) {
                Loc::Param
            } else {
                Loc::Slot
            };
            let entry_slotted = loc == Loc::Slot || (!pend.is_empty() && all_slot);
            plan.push((v, loc, entry_slotted));
        }
        for &(v, loc, _) in &plan {
            if loc == Loc::Param {
                self.body.add_blockparam(wb, machine(&self.ty(v)).unwrap());
            }
        }
        self.plans.insert(b, plan.clone());
        // What may hold a dead value on entry: as the hot edges have it (a
        // helper's slow path clears its own extra on its edge, `conform`,
        // so a hot GC point after the join need not).
        let hot_dirty = pend.iter().any(|p| !p.cold);
        let mut dirty: BTreeSet<u32> = pend
            .iter()
            .filter(|p| !hot_dirty || !p.cold)
            .flat_map(|p| p.dirty.iter().copied())
            .collect();
        // A loop that may GC starts clean: its entry edges clear what they
        // may leave dead (`conform`), so its GC points do not clear them
        // on every iteration.
        // (Only the slots its calls scan, `scc_top`: those above stay
        // unscanned inside the loop, and are dirty again past each call.)
        if !later.is_empty() && !dirty.is_empty() && self.loop_may_gc(b, &later) {
            let kept: BTreeSet<u32> = plan
                .iter()
                .filter(|&&(_, loc, es)| loc == Loc::Slot || es)
                .filter_map(|(v, _, _)| self.home.get(v).copied())
                .collect();
            let scanned = self
                .scc_top
                .get(&b)
                .copied()
                .unwrap_or(self.nslots)
                .min(self.block_slots(b));
            dirty.retain(|&s| kept.contains(&s) || s >= scanned);
        }
        self.entry_dirty.insert(b, dirty.clone());
        for p in pend {
            self.conform(b, p)?;
        }
        // The emission state.
        self.cur = wb;
        let wparams: Vec<Value> = self.body.blocks[wb]
            .params
            .iter()
            .map(|&(_, v)| v)
            .collect();
        let mut k = 0;
        for &p in &f.blocks[b].params {
            if machine(&self.ty(p)).is_some() {
                self.vmap.insert(p, wparams[k]);
                k += 1;
            }
        }
        self.slotted.clear();
        self.dirty = dirty;
        for (v, loc, es) in plan {
            match loc {
                Loc::Param => {
                    self.vmap.insert(v, wparams[k]);
                    k += 1;
                }
                Loc::Direct(w) => {
                    self.vmap.insert(v, w);
                }
                Loc::Slot => {
                    self.vmap.remove(&v);
                }
            }
            if es {
                self.slotted.insert(v);
            }
        }
        self.cold = false;
        Ok(())
    }

    /// Fill edge `p` into entered block `b`: bring each managed live-in to
    /// where `b` expects it, then branch.
    pub(in crate::wasm::mir::lower) fn conform(&mut self, b: mir::Block, p: PendingEdge) -> R<()> {
        let saved = self.cur;
        self.cur = p.tb;
        let mut args = p.args;
        let mut stores = 0u32;
        let mut stored: Vec<String> = vec![];
        for (v, loc, es) in self.plans[&b].clone() {
            let (reg, sl) = p.snap.get(&v).copied().unwrap_or((None, false));
            let lost = || format!("lowering: {v} is live into {b} but has no value on an edge");
            if (loc == Loc::Slot || es) && !sl {
                let r = reg.ok_or_else(lost)?;
                let t = self.ty(v);
                let boxed = self.boxed(&t, r)?;
                let off = self.home_off(v)?;
                self.store_i64(self.vp, off, boxed);
                stores += 1;
                if self.root_census.is_some() {
                    stored.push(format!("{v}:{loc:?}:{es}"));
                }
            }
            match loc {
                Loc::Param => {
                    let r = match reg {
                        Some(r) => r,
                        None if sl => self.load_home(v)?,
                        None => return Err(lost()),
                    };
                    args.push(r);
                }
                Loc::Direct(w) => {
                    if reg != Some(w) {
                        return Err(format!("lowering: {v} enters {b} as two values"));
                    }
                }
                Loc::Slot => {}
            }
        }
        // A slot this edge may have left a dead value in, which the block
        // does not know to clear (a back edge): clear it here, unless it
        // holds a value the block keeps in it.
        let kept: BTreeSet<u32> = self.plans[&b]
            .iter()
            .filter(|&&(_, loc, es)| loc == Loc::Slot || es)
            .filter_map(|(v, _, _)| self.home.get(v).copied())
            .collect();
        // (Never one an inline frame overlays: `block_slots`.)
        let limit = self.block_slots(b);
        let extra: Vec<u32> = p
            .dirty
            .iter()
            .copied()
            .filter(|&s| s < limit && !self.entry_dirty[&b].contains(&s) && !kept.contains(&s))
            .collect();
        if !extra.is_empty() {
            let undef = self.i64c(UNDEF);
            for s in extra {
                self.store_i64(self.vp, self.root_base + 8 * s, undef);
                stores += 1;
            }
        }
        if self.root_census.is_some() {
            let cold = std::mem::replace(&mut self.cold, p.cold);
            self.root_tick(&format!("edge->{b}[{}]", stored.join(",")), stores);
            self.cold = cold;
        }
        let wb = self.blocks[&b];
        self.terminate(Terminator::Br {
            target: BlockTarget { block: wb, args },
        });
        self.cur = saved;
        Ok(())
    }

    /// An edge into MIR block `b` with explicit waffle args `args`, from
    /// the emission point: its own waffle block, filled now if `b` has been
    /// entered (a back edge), else when it is.
    pub(in crate::wasm::mir::lower) fn edge_into(
        &mut self,
        b: mir::Block,
        args: Vec<Value>,
    ) -> R<BlockTarget> {
        let tb = self.body.add_block();
        let mut snap = BTreeMap::new();
        if let Some(l) = self.live_in.get(&b) {
            for &v in l {
                if is_managed(&self.ty(v)) {
                    snap.insert(v, (self.vmap.get(&v).copied(), self.slotted.contains(&v)));
                }
            }
        }
        let p = PendingEdge {
            tb,
            args,
            snap,
            dirty: self.dirty.clone(),
            cold: self.cold,
        };
        if self.plans.contains_key(&b) {
            self.conform(b, p)?;
        } else {
            self.pending.entry(b).or_default().push(p);
        }
        Ok(Self::to(tb))
    }
}
