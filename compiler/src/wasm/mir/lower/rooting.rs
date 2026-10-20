//! rooting: methods of `Lower`, split out of `lower.rs` unchanged.

use super::*;

impl<'a> Lower<'a> {
    /// The managed values live across terminator `inst` (into any
    /// successor), in a fixed order.
    pub(super) fn live_across(&self, inst: mir::Inst) -> Vec<mir::Value> {
        let d = &self.f.insts[inst];
        let mut s: BTreeSet<mir::Value> = BTreeSet::new();
        for e in &d.succs {
            if let Some(l) = self.live_in.get(&e.block) {
                s.extend(l.iter().copied());
            }
            for a in &e.args {
                if let EdgeArg::Value(v) = a {
                    s.insert(*v);
                }
            }
        }
        s.into_iter().filter(|&v| is_managed(&self.ty(v))).collect()
    }

    /// Call may-GC helper `f(cx, top, args...)` with every value in `live`
    /// rooted, reloading them afterwards. Returns the helper's i32 status
    /// and the boxed result it wrote at `top`.
    pub(super) fn gc_call(
        &mut self,
        f: Func,
        args: &[Value],
        live: &[mir::Value],
    ) -> R<(Value, Value)> {
        self.root(live)?;
        let top_off = self.top_off(live.len());
        let top = self.add_off(self.vp, top_off);
        let mut full = vec![self.cx, top];
        full.extend_from_slice(args);
        let pre = self.epoch();
        let ok = self.call1(f, &full, Type::I32);
        let post = self.epoch();
        self.epoch_same = Some(self.bin(Operator::I32Eq, pre, post, Type::I32));
        self.after_gc(live);
        // A helper's slow path: its edges reload what their targets keep
        // in registers.
        self.cold = true;
        let result = self.load_i64(self.vp, top_off);
        Ok((ok, result))
    }

    /// The first free byte above everything a may-GC call's frame chain
    /// holds, as an offset from `sp`: where its helper's out-slot and GC
    /// scan limit, and a real call's frame, go -- as a native stack pointer
    /// would be. Set by the call's `root`: in the function's own frame,
    /// past the rooting slots its live values occupy (`root_top`); in an
    /// inline frame, past that frame, so past every rooting slot and every
    /// enclosing inline frame. (`live` no longer matters: kept for the
    /// callers' shape.)
    pub(super) fn top_off(&self, _live: usize) -> u32 {
        self.gc_top
    }

    /// How many rooting slots a may-GC call with `live` live across it
    /// scans: in the function's own frame, past the highest home slot of
    /// `live`, and at least its cycle's (`scc_top`); in an inline frame,
    /// all of them (its frame is above them).
    pub(super) fn root_top(&self, live: &[mir::Value]) -> u32 {
        if self.cur_frame != 0 {
            return self.inline_k[self.outer_frame(self.cur_frame) as usize];
        }
        let need = live
            .iter()
            .filter_map(|v| self.home.get(v))
            .map(|&k| k + 1)
            .max()
            .unwrap_or(0);
        need.max(self.scc_top.get(&self.cur_mblock).copied().unwrap_or(0))
    }

    /// `scc_top`: the CFG's strongly connected components (Tarjan), and
    /// per component with a cycle, the most rooting slots any may-GC
    /// instruction in it needs (all of them for one in an inline frame).
    pub(super) fn scc_tops(&mut self, order: &[mir::Block]) {
        let f = self.f;
        let blocks: Vec<mir::Block> = order.to_vec();
        let idx: BTreeMap<mir::Block, usize> =
            blocks.iter().enumerate().map(|(i, &b)| (b, i)).collect();
        let succs: Vec<Vec<usize>> = blocks
            .iter()
            .map(|&b| {
                f.succs(b)
                    .into_iter()
                    .filter_map(|s| idx.get(&s).copied())
                    .collect()
            })
            .collect();
        let n = blocks.len();
        let (mut index, mut low, mut on) = (vec![usize::MAX; n], vec![0usize; n], vec![false; n]);
        let (mut stack, mut comps, mut next) = (vec![], vec![], 0usize);
        for root in 0..n {
            if index[root] != usize::MAX {
                continue;
            }
            // Iterative Tarjan: (node, next successor to visit).
            let mut work = vec![(root, 0usize)];
            index[root] = next;
            low[root] = next;
            next += 1;
            stack.push(root);
            on[root] = true;
            while let Some(&mut (v, ref mut k)) = work.last_mut() {
                if *k < succs[v].len() {
                    let w = succs[v][*k];
                    *k += 1;
                    if index[w] == usize::MAX {
                        index[w] = next;
                        low[w] = next;
                        next += 1;
                        stack.push(w);
                        on[w] = true;
                        work.push((w, 0));
                    } else if on[w] {
                        low[v] = low[v].min(index[w]);
                    }
                } else {
                    work.pop();
                    if let Some(&(u, _)) = work.last() {
                        low[u] = low[u].min(low[v]);
                    }
                    if low[v] == index[v] {
                        let mut comp = vec![];
                        loop {
                            let w = stack.pop().unwrap();
                            on[w] = false;
                            comp.push(w);
                            if w == v {
                                break;
                            }
                        }
                        comps.push(comp);
                    }
                }
            }
        }
        for comp in comps {
            let cyclic = comp.len() > 1 || succs[comp[0]].contains(&comp[0]);
            if !cyclic {
                continue;
            }
            let mut top = 0;
            for &c in &comp {
                for &i in &f.blocks[blocks[c]].insts {
                    if !self.may_gc(i) {
                        continue;
                    }
                    let need = if f.inst_frame[i] != 0 {
                        self.inline_k[self.outer_frame(f.inst_frame[i]) as usize]
                    } else {
                        self.live_across(i)
                            .iter()
                            .filter_map(|v| self.home.get(v))
                            .map(|&k| k + 1)
                            .max()
                            .unwrap_or(0)
                    };
                    top = top.max(need);
                }
            }
            for &c in &comp {
                self.scc_top.insert(blocks[c], top);
            }
        }
    }

    /// How many rooting slots a block's code may scan, so may clear: in an
    /// inline frame's region, those below the frame (`inline_k`), which
    /// overlays the rest; elsewhere all of them.
    pub(super) fn block_slots(&self, b: mir::Block) -> u32 {
        match self.f.blocks[b]
            .insts
            .first()
            .map(|&i| self.f.inst_frame[i])
        {
            Some(fr) if fr != 0 => self.inline_k[self.outer_frame(fr) as usize],
            _ => self.nslots,
        }
    }

    /// The inline frame inlined into the function's own frame that `fid`
    /// is in (itself, or an ancestor).
    pub(super) fn outer_frame(&self, mut fid: u32) -> u32 {
        loop {
            let parent = self.f.inline_frames[fid as usize - 1].parent;
            if parent == 0 {
                return fid;
            }
            fid = parent;
        }
    }

    /// `inline_k`: per outermost inline frame, one past the highest home
    /// of a managed value live into a block of its region or across a
    /// may-GC instruction in it.
    pub(super) fn inline_ks(&mut self) {
        let f = self.f;
        let mut k = vec![0u32; f.inline_frames.len() + 1];
        let homed = |l: &Lower, vs: &mut dyn Iterator<Item = mir::Value>| -> u32 {
            vs.filter_map(|v| l.home.get(&v))
                .map(|&h| h + 1)
                .max()
                .unwrap_or(0)
        };
        for b in f.blocks.keys() {
            let mut outers = BTreeSet::new();
            for &i in &f.blocks[b].insts {
                let fr = f.inst_frame[i];
                if fr == 0 {
                    continue;
                }
                let o = self.outer_frame(fr);
                outers.insert(o);
                let n = homed(self, &mut self.live_across(i).into_iter());
                k[o as usize] = k[o as usize].max(n);
            }
            if outers.is_empty() {
                continue;
            }
            let n = self
                .live_in
                .get(&b)
                .map_or(0, |l| homed(self, &mut l.iter().copied()));
            for o in outers {
                k[o as usize] = k[o as usize].max(n);
            }
        }
        self.inline_k = k;
    }

    /// Lay out the inline frames (§5.5): one inlined into the function's
    /// own frame right above the rooting slots its region keeps
    /// (`inline_k`), one inlined into another at its parent's end.
    pub(super) fn inline_layout(&mut self) {
        for (i, fr) in self.f.inline_frames.iter().enumerate() {
            // The callee's own layout (its baseline body resumes on the
            // frame), with the actuals placed by `voff` rather than a rebase.
            let lay = match self.inline_layouts.get(i) {
                Some(&l) => FrameLayout {
                    rebase_vp: false,
                    ..l
                },
                None => FrameLayout::full(fr.shape.formals, fr.shape.locals),
            };
            debug_assert!(lay.nargs == fr.shape.formals && lay.nlocals == fr.shape.locals);
            let off = if fr.parent == 0 {
                self.root_base + 8 * self.inline_k[i + 1]
            } else {
                self.frame_top[fr.parent as usize]
            };
            let voff = off + 8 * fr.argc.map_or(0, |n| n.saturating_sub(fr.shape.formals));
            let fid = u32::try_from(self.frame_off.len()).unwrap();
            let nact = fr.argc.map_or(lay.nargs, |n| n.max(lay.nargs)) as usize;
            let mut fixed = Fixed::None;
            for (i, d) in self.f.insts.iter() {
                if self.f.inst_frame[i] != fid {
                    continue;
                }
                match d.op {
                    Opcode::ArgsObject | Opcode::FrameNewTarget => fixed = Fixed::All,
                    Opcode::InlineEnter if d.args.len() != 2 + nact => fixed = Fixed::All,
                    Opcode::EnvSet | Opcode::EnvPop if fixed == Fixed::None => fixed = Fixed::Env,
                    _ => {}
                }
            }
            self.frame_off.push(off);
            self.frame_voff.push(voff);
            self.frame_end.push(voff + lay.top(fr.max_depth + 3));
            self.frame_top.push(match fixed {
                Fixed::All => voff + lay.operand_base(),
                Fixed::Env => voff + lay.env() + 8,
                Fixed::None => voff + lay.fixed_base(),
            });
            self.fixed.push(fixed);
            self.frame_layouts.push(lay);
        }
    }

    /// The stamp epoch (`gNightStampEpoch`, low word), which every
    /// demotion of a stamped object's class word bumps: unchanged across
    /// a call, no layout fact was invalidated.
    pub(super) fn epoch(&mut self) -> Value {
        let slot = self.i32c(self.h.strlit_slot + crate::region_shape::STRLIT_STAMP_EPOCH_ADDR_OFF);
        let addr = self.load_i32(slot, 0);
        self.load_i32(addr, 0)
    }

    /// Bump the stamp epoch (a demotion in compiled code).
    pub(super) fn bump_epoch(&mut self) {
        let slot = self.i32c(self.h.strlit_slot + crate::region_shape::STRLIT_STAMP_EPOCH_ADDR_OFF);
        let addr = self.load_i32(slot, 0);
        let v = self.load_i64(addr, 0);
        let one = self.i64c(1);
        let n = self.bin(Operator::I64Add, v, one, Type::I64);
        self.store_i64(addr, 0, n);
    }

    /// The success edges of a generic op after its helper `ok`: `ok_clean`
    /// when the stamp epoch did not move across it (`epoch_same`, from
    /// the last `gc_call` or a call's own sample), so its layout facts
    /// hold; `ok_dirty` otherwise; `err` on failure.
    pub(super) fn clean_or_dirty(
        &mut self,
        inst: mir::Inst,
        ok: Value,
        same: Value,
        outs: &[Value],
    ) -> R<()> {
        let e = self.edge(inst, 2, &[])?;
        let okb = self.body.add_block();
        self.cond_br(ok, Self::to(okb), e);
        self.cur = okb;
        let c = self.edge(inst, 0, outs)?;
        let d = self.edge(inst, 1, outs)?;
        self.cond_br(same, c, d);
        Ok(())
    }

    /// Make the rooting area valid Values: every may-GC call's scan covers
    /// all of it, whatever is live.
    pub(super) fn init_root_area(&mut self) {
        if self.nslots == 0 {
            return;
        }
        let undef = self.i64c(UNDEF);
        for i in 0..self.nslots {
            self.store_i64(self.vp, self.root_base + 8 * i, undef);
        }
    }

    /// Write the pending retaining stores (the locals' values, into the
    /// frame the GC sees: a value stays alive while its local holds it, as
    /// in baseline), but not where both it and what the frame slot holds
    /// are rooted anyway, in `live` (so in their home slots). Returns the
    /// stores made.
    pub(super) fn flush_retain(&mut self, live: Option<&[mir::Value]>) -> R<u32> {
        let mut stores = 0;
        for (key, v) in self.pending_retain.clone() {
            if self.framed.get(&key) == Some(&v) {
                continue;
            }
            if let Some(live) = live {
                if live.contains(&v) && self.framed.get(&key).is_some_and(|o| live.contains(o)) {
                    continue;
                }
            }
            let (fid, k) = key;
            let f = fid as usize;
            let l = self.frame_layouts[f];
            let (nargs, nlocals) = (l.nargs, l.nlocals);
            let off = match k {
                0 => self.frame_off[f] + FrameLayout::THIS,
                k if k <= nargs => self.frame_off[f] + l.arg(k - 1),
                k if k <= nargs + nlocals => self.frame_voff[f] + l.local(k - 1 - nargs),
                _ => self.frame_voff[f] + l.rval(),
            };
            // A slot past the frame's GC scan (a lean inline frame's rval)
            // retains nothing, and a nested inline frame may occupy it.
            if f > 0 && off >= self.frame_top[f] {
                continue;
            }
            let w = self.value_here(v)?;
            let t = self.ty(v);
            let b = self.boxed(&t, w)?;
            let base = if fid == 0 && k <= nargs {
                self.sp
            } else {
                self.vp
            };
            self.store_i64(base, off, b);
            self.framed.insert(key, v);
            stores += 1;
        }
        Ok(stores)
    }

    /// Before a may-GC call: store each of `live` (managed values live
    /// across it) that its home slot does not hold yet. A stored value
    /// stays stored (SSA values do not change, and the GC updates the slot
    /// in place), so repeated calls store it once.
    pub(super) fn root(&mut self, live: &[mir::Value]) -> R<()> {
        let mut stores = self.flush_retain(Some(live))?;
        for &v in live {
            if self.slotted.contains(&v) {
                continue;
            }
            stores += 1;
            let t = self.ty(v);
            let w = self.get(v)?;
            let b = self.boxed(&t, w)?;
            let off = self.home_off(v)?;
            self.store_i64(self.vp, off, b);
            self.slotted.insert(v);
        }
        // The scan: the rooting slots below `top`, and the frame below
        // them. Slots there holding what is live now are valid; any other
        // dirty one may hold a dead frame's or stale bits, which the GC
        // must not see.
        let top = self.root_top(live);
        self.gc_top = if self.cur_frame != 0 {
            self.frame_top[self.cur_frame as usize]
        } else {
            self.root_base + 8 * top
        };
        let holding: BTreeSet<u32> = live
            .iter()
            .filter_map(|v| self.home.get(v).copied())
            .collect();
        let stale: Vec<u32> = self
            .dirty
            .iter()
            .copied()
            .filter(|&s| s < top && !holding.contains(&s))
            .collect();
        if !stale.is_empty() {
            let undef = self.i64c(UNDEF);
            for s in &stale {
                self.store_i64(self.vp, self.root_base + 8 * s, undef);
            }
            stores += u32::try_from(stale.len()).unwrap();
        }
        // Every slot below `top` is valid now (a dead value may stay in
        // its slot); those above are unscanned, and the call's frame or
        // out-slot may overwrite them.
        self.dirty = (top..self.nslots).collect();
        self.slotted.retain(|v| live.contains(v));
        let e = self
            .opsize
            .entry("(root sites / stores)".into())
            .or_default();
        e.0 += 1;
        e.1 += stores;
        self.root_tick("root", stores);
        Ok(())
    }

    /// `--root-census`: count this rooting site's executions, where it
    /// stored anything, and print its static record.
    pub(super) fn root_tick(&mut self, what: &str, stores: u32) {
        let Some(census) = self.root_census else {
            return;
        };
        if stores == 0 {
            return;
        }
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let op = self.cur_inst.map_or("-".to_string(), |i| {
            mir::print::mnemonic(&self.f.insts[i].op)
        });
        crate::diag_line!(
            "night: mir root {id} sid#{} {what}@{} {op} stores {stores} cold {}",
            self.f.script,
            self.cur_mblock,
            self.cold
        );
        let (k, i) = (
            self.i32c(crate::options::MIR_ROOT_CENSUS_KIND),
            self.i32c(id),
        );
        self.call1(census, &[k, i], Type::I32);
    }

    /// After a may-GC call: `live`'s register copies may point to moved
    /// objects; each is reloaded from its home slot where next used.
    pub(super) fn after_gc(&mut self, live: &[mir::Value]) {
        for v in live {
            self.vmap.remove(v);
        }
    }
}
