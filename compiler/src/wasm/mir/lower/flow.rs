//! flow: methods of `Lower`, split out of `lower.rs` unchanged.

use super::*;

impl<'a> Lower<'a> {
    /// Blocks reachable from a root.
    /// The reachable blocks in reverse postorder from the roots.
    pub(super) fn rpo(&self) -> Vec<mir::Block> {
        let mut seen = BTreeSet::new();
        let mut post = vec![];
        for r in &self.f.roots {
            if !seen.insert(r.block) {
                continue;
            }
            let mut stack = vec![(r.block, self.f.succs(r.block), 0usize)];
            while let Some((b, succs, i)) = stack.last_mut() {
                if *i < succs.len() {
                    let s = succs[*i];
                    *i += 1;
                    if seen.insert(s) {
                        let ss = self.f.succs(s);
                        stack.push((s, ss, 0));
                    }
                } else {
                    post.push(*b);
                    stack.pop();
                }
            }
        }
        post.reverse();
        post
    }

    pub(super) fn reachable(&self) -> BTreeSet<mir::Block> {
        let mut seen = BTreeSet::new();
        let mut work: Vec<mir::Block> = self.f.roots.iter().map(|r| r.block).collect();
        while let Some(b) = work.pop() {
            if seen.insert(b) {
                work.extend(self.f.succs(b));
            }
        }
        seen
    }

    pub(super) fn liveness(&mut self, blocks: &BTreeSet<mir::Block>) {
        let f = self.f;
        let edge_uses = |inst: mir::Inst| -> Vec<mir::Value> {
            f.insts[inst]
                .succs
                .iter()
                .flat_map(|e| e.args.iter())
                .filter_map(|a| match a {
                    EdgeArg::Value(v) => Some(*v),
                    EdgeArg::Out(_) => None,
                })
                .collect()
        };
        let mut changed = true;
        while changed {
            changed = false;
            for &b in blocks.iter().rev() {
                let mut live: BTreeSet<mir::Value> = BTreeSet::new();
                for s in f.succs(b) {
                    if let Some(l) = self.live_in.get(&s) {
                        live.extend(l.iter().copied());
                    }
                }
                for &inst in f.blocks[b].insts.iter().rev() {
                    for r in &f.insts[inst].results {
                        live.remove(r);
                    }
                    live.extend(f.insts[inst].args.iter().copied());
                    live.extend(edge_uses(inst));
                }
                for p in &f.blocks[b].params {
                    live.remove(p);
                }
                live.retain(|&v| machine(&f.values[v].ty).is_some());
                if self.live_in.get(&b) != Some(&live) {
                    self.live_in.insert(b, live);
                    changed = true;
                }
            }
        }
    }

    pub(super) fn run(&mut self) -> R<()> {
        let f = self.f;
        let reach = self.reachable();
        self.liveness(&reach);
        let order = self.rpo();
        for (i, &b) in order.iter().enumerate() {
            self.rpo_index.insert(b, i);
            for s in f.succs(b) {
                self.preds.entry(s).or_default().push(b);
            }
        }
        self.homes(&reach, &order);
        self.frame_end[0] = self.root_base + 8 * self.nslots;
        self.frame_top[0] = self.frame_end[0];
        self.inline_ks();
        self.scc_tops(&order);
        // A region in a cycle overlays only what the cycle's calls do not
        // scan: else each entry of it would dirty slots the cycle's other
        // calls then clear again, every iteration.
        for b in f.blocks.keys() {
            let Some(&top) = self.scc_top.get(&b) else {
                continue;
            };
            for &i in &f.blocks[b].insts {
                let fr = f.inst_frame[i];
                if fr != 0 {
                    let o = self.outer_frame(fr) as usize;
                    self.inline_k[o] = self.inline_k[o].max(top);
                }
            }
        }
        if self.inline {
            self.inline_layout();
        }
        // Waffle blocks with the block's own params; its managed live-ins
        // are placed when it is entered.
        for &b in &f.layout {
            if !reach.contains(&b) {
                continue;
            }
            let wb = self.body.add_block();
            for &p in &f.blocks[b].params {
                if let Some(t) = machine(&self.ty(p)) {
                    self.body.add_blockparam(wb, t);
                }
            }
            self.blocks.insert(b, wb);
        }
        self.value_tag("(entry)");
        self.entry()?;
        // In reverse postorder, so a block's dominators, which define the
        // unmanaged values it uses directly, are lowered before it.
        for b in order {
            self.cur_mblock = b;
            self.value_tag("(edges)");
            self.enter_block(b)?;
            if let Some(census) = self.block_census {
                self.value_tag("(census)");
                static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
                let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let ops: Vec<String> = f.blocks[b]
                    .insts
                    .iter()
                    .map(|&i| mir::print::mnemonic(&f.insts[i].op))
                    .collect();
                crate::diag_line!(
                    "night: mir block {id} sid#{} {b} {}",
                    self.f.script,
                    ops.join(" ")
                );
                let (k, i) = (
                    self.i32c(crate::options::MIR_BLOCK_CENSUS_KIND),
                    self.i32c(id),
                );
                self.call1(census, &[k, i], Type::I32);
                self.value_tag("(edges)");
            }
            self.block_enters = None;
            for &inst in &f.blocks[b].insts {
                self.cold = false;
                self.cur_inst = Some(inst);
                if matches!(f.insts[inst].op, Opcode::InlineEnter) {
                    self.block_enters = Some(f.inst_frame[inst]);
                }
                let before = self.body.values.len();
                if self.viz_marks.is_some() && self.value_census.is_none() {
                    self.value_tag("inst");
                }
                if self.value_census.is_some() {
                    let mut tag = mir::print::mnemonic(&f.insts[inst].op);
                    if let Some(site) = f.insts[inst].attach.and_then(|a| f.attachments[a].site) {
                        tag = format!("{tag}@{site}");
                    } else if let Some(pc) = self.exit_pc(inst) {
                        let fr = f.inst_frame[inst];
                        let script = if fr == 0 {
                            f.script
                        } else {
                            f.inline_frames[fr as usize - 1].script
                        };
                        tag = format!("{tag}@{script}:{pc}");
                    }
                    self.value_tag(&tag);
                }
                self.inst(inst)?;
                let e = self
                    .opsize
                    .entry(mir::print::mnemonic(&f.insts[inst].op))
                    .or_default();
                e.0 += 1;
                e.1 += u32::try_from(self.body.values.len() - before).unwrap();
            }
        }
        if let Some((b, _)) = self.pending.iter().find(|(_, v)| !v.is_empty()) {
            return Err(format!("lowering: an edge into {b} was never placed"));
        }
        self.value_tag("(hubs)");
        // Each `exit.inline` hub continues at its site's tail.
        for (_, last, site, err, same, tails) in std::mem::take(&mut self.inline_hubs).into_values()
        {
            let targets: Vec<BlockTarget> = tails
                .iter()
                .map(|&t| BlockTarget {
                    block: t,
                    args: vec![err, same],
                })
                .collect();
            let default = targets[0].clone();
            self.body.set_terminator(
                last,
                Terminator::Select {
                    value: site,
                    targets,
                    default,
                },
            );
        }
        waffle::passes::empty_blocks::run(&mut self.body);
        self.instrument_values();
        Ok(())
    }

    /// The pc an instruction exits at on failure, where a successor is an
    /// exit block (the op's own bytecode pc): for `--value-census` tags.
    pub(super) fn exit_pc(&self, inst: mir::Inst) -> Option<crate::ids::Pc> {
        self.f.insts[inst].succs.iter().find_map(|e| {
            let t = self.f.terminator(e.block)?;
            match self.f.insts[t].op {
                Opcode::Exit { pc, .. }
                | Opcode::ExitThrow { pc, .. }
                | Opcode::ExitInline { pc, .. } => Some(pc),
                _ => None,
            }
        })
    }

    /// `--value-census`: the Wasm values from here on are `tag`'s.
    pub(super) fn value_tag(&mut self, tag: &str) {
        // `--viz`: an instruction's own tag marks it (the loop sets
        // `cur_inst` first); any other starts unattributed values.
        if let Some(marks) = self.viz_marks.as_mut() {
            let i = match (tag.starts_with('('), self.cur_inst) {
                (false, Some(inst)) => mir::entity::EntityRef::index(inst) as u32 + 1,
                _ => 0,
            };
            marks.push((self.body.values.len(), i));
        }
        if self.value_census.is_some() {
            self.value_tags
                .push((self.body.values.len(), tag.to_string()));
        }
    }

    /// `--value-census`: a census tick at the head of every Wasm block,
    /// and its static record of the values in it, by emitter.
    pub(super) fn instrument_values(&mut self) {
        let Some(census) = self.value_census else {
            return;
        };
        let tags = std::mem::take(&mut self.value_tags);
        let tag_of = |v: Value| -> &str {
            let i = v.index();
            match tags.partition_point(|&(s, _)| s <= i) {
                0 => "(other)",
                k => &tags[k - 1].1,
            }
        };
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let blocks: Vec<Block> = self.body.blocks.iter().collect();
        for b in blocks {
            let mut n: BTreeMap<&str, u32> = BTreeMap::new();
            for &v in &self.body.blocks[b].insts {
                *n.entry(tag_of(v)).or_default() += 1;
            }
            if n.is_empty() {
                continue;
            }
            let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let rec: Vec<String> = n.iter().map(|(t, c)| format!("{t}:{c}")).collect();
            crate::diag_line!(
                "night: mir vblock {id} sid#{} {}",
                self.f.script,
                rec.join(" ")
            );
            let len = self.body.blocks[b].insts.len();
            let saved = self.cur;
            self.cur = b;
            let (k, i) = (
                self.i32c(crate::options::MIR_VALUE_CENSUS_KIND),
                self.i32c(id),
            );
            self.call1(census, &[k, i], Type::I32);
            self.cur = saved;
            // The tick first, before the block's own instructions.
            self.body.blocks[b].insts.rotate_left(len);
        }
    }

    /// Whether an instruction in the loop through header `h` (back-edge
    /// sources `later`) may GC.
    pub(super) fn loop_may_gc(&self, h: mir::Block, later: &[mir::Block]) -> bool {
        let f = self.f;
        let mut fwd: BTreeSet<mir::Block> = BTreeSet::new();
        let mut work = vec![h];
        while let Some(b) = work.pop() {
            if fwd.insert(b) {
                work.extend(f.succs(b));
            }
        }
        let mut body: BTreeSet<mir::Block> = BTreeSet::new();
        let mut work: Vec<mir::Block> = later.to_vec();
        while let Some(b) = work.pop() {
            if fwd.contains(&b) && body.insert(b) && b != h {
                work.extend(self.preds.get(&b).cloned().unwrap_or_default());
            }
        }
        body.iter()
            .any(|&b| f.blocks[b].insts.iter().any(|&i| self.may_gc(i)))
    }

    /// Whether `inst` may call something that GCs (a superset of where the
    /// lowering roots: any op with an `err` or `ok_dirty` edge).
    pub(super) fn may_gc(&self, inst: mir::Inst) -> bool {
        use mir::ops::SuccRole;
        let op = &self.f.insts[inst].op;
        // `init_field` (ok, fail) calls the runtime's plain add on its
        // slow path.
        matches!(op, Opcode::InitField(_) | Opcode::StoreElem(_, true))
            || op
                .roles()
                .iter()
                .any(|r| matches!(r, SuccRole::Err | SuccRole::OkDirty))
    }

    /// Home slots: every managed value live across a may-GC instruction
    /// gets one, shared among values never live at once. In SSA two
    /// values interfere iff one is live at the other's definition, so
    /// coloring in definition order (reverse postorder, which respects
    /// dominance) against the colored values live there is enough. Also
    /// computes `crossed_out`.
    pub(super) fn homes(&mut self, reach: &BTreeSet<mir::Block>, order: &[mir::Block]) {
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
    pub(super) fn enter_block(&mut self, b: mir::Block) -> R<()> {
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
    pub(super) fn conform(&mut self, b: mir::Block, p: PendingEdge) -> R<()> {
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
    pub(super) fn edge_into(&mut self, b: mir::Block, args: Vec<Value>) -> R<BlockTarget> {
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

    /// The entry. Under `ARGC_ONRAMP_BIT` (baseline at a loop header),
    /// enter the onramp root the resume word names, with its params read
    /// from the baseline frame. Otherwise, pad the formals the caller did
    /// not pass with undefined (the frame below the rooting slots must hold
    /// valid Values), then enter the entry root with callee, `this` and the
    /// formals.
    pub(super) fn entry(&mut self) -> R<()> {
        if self.forward_resume {
            // An inlined copy's exit finishes the call in baseline through
            // this entry (§5.5): hand the frame, as it is, to the baseline
            // body.
            let rb = self.i32c(ARGC_RESUME_BIT);
            let resume = self.bin(Operator::I32And, self.argc, rb, Type::I32);
            let (fwd, rest) = (self.body.add_block(), self.body.add_block());
            self.cond_br(resume, Self::to(fwd), Self::to(rest));
            self.cur = fwd;
            let argc = self.argc;
            self.tail_to_baseline(argc);
            self.cur = rest;
        }
        let bit = self.i32c(ARGC_ONRAMP_BIT);
        self.onramp_flag = self.bin(Operator::I32And, self.argc, bit, Type::I32);
        let flags = self.i32c(!ARGC_FLAGS);
        self.argc = self.bin(Operator::I32And, self.argc, flags, Type::I32);
        if self.layout.rebase_vp {
            // Past the actuals beyond the formals, as baseline's `vp`.
            let n = self.i32c(self.layout.nargs);
            let extra = self.bin(Operator::I32Sub, self.argc, n, Type::I32);
            let more = self.bin(Operator::I32GtU, self.argc, n, Type::I32);
            let zero = self.i32c(0);
            let extra = self.select(Type::I32, extra, zero, more);
            let eight = self.i32c(8);
            let bytes = self.bin(Operator::I32Mul, extra, eight, Type::I32);
            self.vp = self.bin(Operator::I32Add, self.sp, bytes, Type::I32);
        }
        let onramps: Vec<(Pc, mir::Block)> = self
            .f
            .roots
            .iter()
            .filter_map(|r| match r.kind {
                RootKind::Onramp(pc) => Some((pc, r.block)),
                RootKind::Entry | RootKind::Resume { .. } => None,
            })
            .collect();
        if !onramps.is_empty() {
            let disp = self.body.add_block();
            let fresh = self.body.add_block();
            self.cond_br(self.onramp_flag, Self::to(disp), Self::to(fresh));
            self.cur = disp;
            let word = self.load_i32(self.vp, self.layout.resume());
            for (pc, root) in onramps {
                let w = ResumeWord {
                    pc,
                    mode: ResumeMode::Continue,
                };
                let k = self.i32c(w.encode() as u32);
                let hit = self.bin(Operator::I32Eq, word, k, Type::I32);
                let (yes, no) = (self.body.add_block(), self.body.add_block());
                self.cond_br(hit, Self::to(yes), Self::to(no));
                self.cur = yes;
                self.enter_onramp(pc, root)?;
                self.cur = no;
            }
            self.terminate(Terminator::Unreachable);
            self.cur = fresh;
        }
        if self.is_gen {
            // A generator's resume stages the generator-closing magic as
            // `this`, which no call passes (`EnterNightResume`).
            let thisv = self.load_i64(self.sp, FrameLayout::THIS);
            let magic = self.i64c((TAG_MAGIC << 32) | MAGIC_GENERATOR_CLOSING);
            let is_resume = self.bin(Operator::I64Eq, thisv, magic, Type::I32);
            let (res, fresh) = (self.body.add_block(), self.body.add_block());
            self.cond_br(is_resume, Self::to(res), Self::to(fresh));
            self.cur = res;
            self.gen_resume_dispatch()?;
            self.cur = fresh;
        }
        let root = self
            .f
            .roots
            .iter()
            .find(|r| r.kind == RootKind::Entry)
            .ok_or("lowering: no entry root")?
            .block;
        let undef = self.i64c(UNDEF);
        let mut vals = vec![
            self.load_i64(self.sp, FrameLayout::CALLEE),
            self.load_i64(self.sp, FrameLayout::THIS),
        ];
        for i in 0..self.layout.nargs {
            let off = self.layout.arg(i);
            let cur = self.load_i64(self.sp, off);
            let iv = self.i32c(i);
            let keep = self.bin(Operator::I32LtU, iv, self.argc, Type::I32);
            let v = self.select(Type::I64, cur, undef, keep);
            self.store_i64(self.sp, off, v);
            vals.push(v);
        }
        // The rest of the baseline frame, as its prologue would set it: the
        // GC traces it (it is below every `top` MIR publishes), and exits
        // leave dead slots as they find them.
        let l = self.layout;
        let vp = self.vp;
        for j in 0..l.nlocals {
            self.store_i64(vp, l.local(j), undef);
        }
        if l.has_args_obj {
            self.store_i64(vp, l.args_obj(), undef);
        }
        self.store_i64(vp, l.rval(), undef);
        // The environment: the callee's own, or none (§5.1).
        let env = if self.plain_env {
            let callee = self.load_i64(self.sp, 0);
            let f = self.un(Operator::I32WrapI64, callee, Type::I32);
            self.load_i64(f, FUNC_ENV_SLOT_OFFSET)
        } else {
            undef
        };
        if l.has_env {
            self.store_i64(vp, l.env(), env);
        }
        if l.has_new_target {
            self.store_i64(vp, l.new_target(), self.new_target);
        }
        let zero = self.i64c(TAG_INT32 << 32);
        self.store_i64(vp, l.resume(), zero);
        self.store_i64(vp, l.backoff(), zero);
        if self.is_gen {
            self.init_root_area();
        } else {
            // Not written here: every slot may hold a dead frame's value,
            // which the first may-GC call on each path clears (`root`'s
            // stale slots), as a loop's entry edges do where the loop may
            // GC (`enter_block`).
            self.dirty = (0..self.nslots).collect();
        }
        if self.own_env {
            // The CallObject inline from the script's row, where it is
            // armed: no GC, so nothing is rooted and the frame's values
            // stand. Else the helper, which fills the row.
            let cell = self.i32c(CONSTRUCT_CELL_ADDR_PLACEHOLDER);
            let idx = self.atoms.next_construct_cell();
            self.construct_cell_patches.push((cell, idx + 1));
            let slow = self.body.add_block();
            let env = self.env_inline(cell, slow);
            self.store_i64(vp, l.env(), env);
            let dirty = self.dirty.clone();
            self.enter_entry_root(root, &vals)?;
            self.cur = slow;
            self.dirty = dirty;
            // The GC may run: nothing is live yet, so its scan stops at the
            // fixed frame, below every rooting slot. Failing, the throw has
            // no handler (baseline's prologue: pc 0, depth 0).
            self.gc_top = self.root_base;
            let top = self.add_off(self.vp, self.top_off(0));
            let script = self.script_ptr();
            let ok = self.call1(
                self.h.env_setup,
                &[self.cx, top, self.sp, script, cell],
                Type::I32,
            );
            let (made, fail) = (self.body.add_block(), self.body.add_block());
            self.cond_br(ok, Self::to(made), Self::to(fail));
            self.cur = fail;
            let one = self.i32c(1);
            self.ret(one);
            self.cur = made;
            let env = self.load_i64(top, 0);
            self.store_i64(vp, l.env(), env);
            // The GC updated the frame, not the values read from it before.
            vals = vec![
                self.load_i64(self.sp, FrameLayout::CALLEE),
                self.load_i64(self.sp, FrameLayout::THIS),
            ];
            for i in 0..self.layout.nargs {
                vals.push(self.load_i64(self.sp, self.layout.arg(i)));
            }
        }
        self.enter_entry_root(root, &vals)
    }

    /// The entry's last step: enter the entry root with callee, `this`
    /// and the formals, `vals`.
    pub(super) fn enter_entry_root(&mut self, root: mir::Block, vals: &[Value]) -> R<()> {
        let params = self.f.blocks[root].params.clone();
        if params.len() != vals.len() {
            return Err("lowering: the entry root's params are not the frame".into());
        }
        let mut args = vec![];
        for (&p, &v) in params.iter().zip(vals) {
            let t = self.ty(p);
            if machine(&t).is_some() {
                args.push(self.unboxed_managed(&t, v));
            }
        }
        let t = self.edge_into(root, args)?;
        self.terminate(Terminator::Br { target: t });
        Ok(())
    }

    /// Enter onramp root `root` (loop header `pc`) with the baseline
    /// frame's state at `pc`: `this`, formals, locals, rval and the
    /// operand stack.
    /// A generator's resume (baseline's `finalize_gen_dispatch`): read
    /// the descriptor `EnterNightResume` staged over the locals, set the
    /// fixed slots a fresh prologue would, restore the locals, operands
    /// and environment from the generator, push the resume protocol's
    /// `[sent value, generator, resume kind]` at the landing's depth, and
    /// enter the `Resume` root for the saved resume index.
    pub(super) fn gen_resume_dispatch(&mut self) -> R<()> {
        let l = self.layout;
        let vp = self.vp;
        let desc = self.add_off(vp, l.local_base());
        let ridx = self.load_i32(desc, 0);
        let roots: Vec<(u32, Pc, mir::Block)> = self
            .f
            .roots
            .iter()
            .filter_map(|r| match r.kind {
                RootKind::Resume { index, pc } => Some((index, pc, r.block)),
                _ => None,
            })
            .collect();
        // A yield this body has no root for (one in catch code, which is
        // baseline's, suspended there after an exit): baseline's own
        // resume dispatch, with the descriptor untouched.
        let other = self.body.add_block();
        let max_k = roots.iter().map(|&(k, _, _)| k).max().unwrap_or(0);
        let mut targets = vec![Self::to(other); (max_k + 1) as usize];
        let disp = self.cur;
        for (k, pc, root) in roots {
            let b = self.body.add_block();
            targets[k as usize] = Self::to(b);
            self.cur = b;
            let desc = self.add_off(vp, l.local_base());
            let rkind = self.load_i32(desc, 4);
            let rgen = self.load_i64(desc, 8);
            let rarg = self.load_i64(desc, 16);
            let undef = self.i64c(UNDEF);
            let mut fixed = l.optional_slots();
            fixed.push(l.rval());
            for off in fixed {
                self.store_i64(vp, off, undef);
            }
            let zero = self.i64c(TAG_INT32 << 32);
            self.store_i64(vp, l.resume(), zero);
            self.store_i64(vp, l.backoff(), zero);
            let lp = self.add_off(vp, l.local_base());
            let nl = self.i32c(l.nlocals);
            let ep = if self.plain_env || self.own_env {
                self.add_off(vp, l.env())
            } else {
                self.i32c(0)
            };
            let ops = self.add_off(vp, l.operand_base());
            self.call(
                self.h.gen_restore,
                &[self.cx, rgen, lp, nl, ep, ops],
                &[Type::I32],
            );
            let depth = *self
                .f
                .frame
                .depths
                .get(&pc)
                .ok_or("lowering: no depth at a resume root")?;
            let saved = depth
                .checked_sub(3)
                .ok_or("lowering: a resume landing below depth 3")?;
            self.store_i64(vp, l.operand(saved), rarg);
            self.store_i64(vp, l.operand(saved + 1), rgen);
            let kind = self.box_tagged(TAG_INT32, rkind);
            self.store_i64(vp, l.operand(saved + 2), kind);
            self.enter_onramp(pc, root)?;
        }
        self.cur = disp;
        let known: Vec<(u32, BlockTarget)> = targets
            .iter()
            .enumerate()
            .filter(|(_, t)| t.block != other)
            .map(|(k, t)| (u32::try_from(k).unwrap(), t.clone()))
            .collect();
        if known.len() <= RESUME_CHAIN_MAX {
            // A few yields (the usual generator, an async function's
            // awaits): compares, each a predicted branch, rather than a
            // `br_table`'s bounds check and indirect jump.
            for (k, t) in known {
                let kv = self.i32c(k);
                let hit = self.bin(Operator::I32Eq, ridx, kv, Type::I32);
                let next = self.body.add_block();
                self.cond_br(hit, t, Self::to(next));
                self.cur = next;
            }
            self.terminate(Terminator::Br {
                target: Self::to(other),
            });
        } else {
            self.terminate(Terminator::Select {
                value: ridx,
                targets,
                default: Self::to(other),
            });
        }
        self.cur = other;
        let argc = self.argc;
        self.tail_to_baseline(argc);
        Ok(())
    }

    pub(super) fn enter_onramp(&mut self, pc: Pc, root: mir::Block) -> R<()> {
        let l = self.layout;
        let (sp, vp) = (self.sp, self.vp);
        let depth = *self
            .f
            .frame
            .depths
            .get(&pc)
            .ok_or("lowering: no depth at an onramp")?;
        let mut vals = vec![self.load_i64(sp, FrameLayout::THIS)];
        for i in 0..l.nargs {
            vals.push(self.load_i64(sp, l.arg(i)));
        }
        for j in 0..l.nlocals {
            vals.push(self.load_i64(vp, l.local(j)));
        }
        vals.push(self.load_i64(vp, l.rval()));
        for k in 0..depth {
            vals.push(self.load_i64(vp, l.operand(k)));
        }
        if vals.len() != self.f.blocks[root].params.len() {
            return Err("lowering: an onramp root's params are not the frame".into());
        }
        // After reading the operands: the rooting area overlaps them.
        self.init_root_area();
        let t = self.edge_into(root, vals)?;
        self.terminate(Terminator::Br { target: t });
        Ok(())
    }

    pub(super) fn to(block: Block) -> BlockTarget {
        BlockTarget {
            block,
            args: vec![],
        }
    }

    /// The waffle target for MIR edge `e`, with `outs` standing for the
    /// terminator's outputs.
    pub(super) fn target(&mut self, e: &Edge, outs: &[Value]) -> R<BlockTarget> {
        if !self.blocks.contains_key(&e.block) {
            return Err(format!("lowering: {} is unreachable", e.block));
        }
        let mut args = vec![];
        let params = self.f.blocks[e.block].params.clone();
        for (a, &p) in e.args.iter().zip(&params) {
            if machine(&self.ty(p)).is_none() {
                continue;
            }
            args.push(match *a {
                EdgeArg::Value(v) => self.value_here(v)?,
                EdgeArg::Out(k) => *outs
                    .get(k as usize)
                    .ok_or_else(|| format!("lowering: no output %{k}"))?,
            });
        }
        self.edge_into(e.block, args)
    }

    /// Branch to MIR edge `e` from a fresh block, returning that block's
    /// target (for a `CondBr` arm that must carry args).
    pub(super) fn edge(&mut self, inst: mir::Inst, k: usize, outs: &[Value]) -> R<BlockTarget> {
        let e = self.f.insts[inst].succs[k].clone();
        self.target(&e, outs)
    }
}
