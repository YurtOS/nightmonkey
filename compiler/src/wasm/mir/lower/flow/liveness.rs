//! liveness: methods of `Lower`, split out of `flow.rs` unchanged.

use super::*;

impl<'a> Lower<'a> {
    /// Blocks reachable from a root.
    /// The reachable blocks in reverse postorder from the roots.
    pub(in crate::wasm::mir::lower) fn rpo(&self) -> Vec<mir::Block> {
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

    pub(in crate::wasm::mir::lower) fn reachable(&self) -> BTreeSet<mir::Block> {
        let mut seen = BTreeSet::new();
        let mut work: Vec<mir::Block> = self.f.roots.iter().map(|r| r.block).collect();
        while let Some(b) = work.pop() {
            if seen.insert(b) {
                work.extend(self.f.succs(b));
            }
        }
        seen
    }

    pub(in crate::wasm::mir::lower) fn liveness(&mut self, blocks: &BTreeSet<mir::Block>) {
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

    /// Whether an instruction in the loop through header `h` (back-edge
    /// sources `later`) may GC.
    pub(in crate::wasm::mir::lower) fn loop_may_gc(
        &self,
        h: mir::Block,
        later: &[mir::Block],
    ) -> bool {
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
    pub(in crate::wasm::mir::lower) fn may_gc(&self, inst: mir::Inst) -> bool {
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
}
