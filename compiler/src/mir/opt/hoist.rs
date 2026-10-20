//! hoist: passes of `opt.rs`, split out unchanged.

use super::*;

/// Loop-invariant code motion (MIR.md §10.2, MIR-MEMORY.md §2, over
/// effects): a non-terminator in a loop that writes nothing, cannot GC,
/// throw or kill facts, reads only regions no instruction in the loop may
/// write (on an edge that stays in the loop), and whose operands are all
/// defined outside the loop (or hoisted), moves to the end of the loop's
/// preheader. Such an op is safe to run once before the loop even where
/// the loop would not have reached it (a `load_slot`'s receiver type
/// proves the slot). Constants stay (nothing to save). Returns how many
/// moved.
pub fn licm(m: &Module, f: &mut Func) -> usize {
    let cfg = Cfg::new(f);
    let mut preds: BTreeMap<Block, Vec<Block>> = BTreeMap::new();
    for &b in &f.layout {
        for s in f.succs(b) {
            preds.entry(s).or_default().push(b);
        }
    }
    let mut inst_block = BTreeMap::new();
    for &b in &f.layout {
        for &i in &f.blocks[b].insts {
            inst_block.insert(i, b);
        }
    }
    let mut moved = 0;
    for l in f.loops.clone() {
        let (h, p) = (l.header, l.preheader);
        // The natural loop: back from the header's in-loop predecessors.
        let mut body: BTreeSet<Block> = BTreeSet::new();
        body.insert(h);
        let mut work: Vec<Block> = preds
            .get(&h)
            .map(|ps| {
                ps.iter()
                    .copied()
                    .filter(|&q| cfg.dominates(h, q))
                    .collect()
            })
            .unwrap_or_default();
        // Unreachable blocks (a folded guard's former successor) are no
        // part of it.
        while let Some(b) = work.pop() {
            if body.insert(b) {
                work.extend(
                    preds
                        .get(&b)
                        .into_iter()
                        .flatten()
                        .copied()
                        .filter(|&q| cfg.reachable(q)),
                );
            }
        }
        // An onramp into a loop nested in this one enters its body past
        // the preheader: then nothing can be hoisted there.
        if body.contains(&p)
            || f.terminator(p).is_none()
            || !body.iter().all(|&b| cfg.dominates(p, b))
        {
            continue;
        }
        // What the loop may write, per edge: a dirty or err edge that
        // stays in the loop wrote whatever the engine ran.
        let mut writes = vec![];
        for &b in &body {
            for &i in &f.blocks[b].insts {
                let d = &f.insts[i];
                let tys: Vec<Type> = d.args.iter().map(|&v| f.values[v].ty).collect();
                let fx = effects(&d.op, &tys, m);
                if d.succs.is_empty() {
                    writes.extend(crate::mir::mem::writes_on(f, m, i, &fx, None));
                }
                for (role, e) in f.succ_edges(i) {
                    if body.contains(&e.block) {
                        writes.extend(crate::mir::mem::writes_on(f, m, i, &fx, Some(role)));
                    }
                }
            }
        }
        let mut hoisted: BTreeSet<Value> = BTreeSet::new();
        let outside = |v: Value, hoisted: &BTreeSet<Value>| {
            hoisted.contains(&v)
                || match f.values[v].def {
                    ValueDef::Param(b, _) => !body.contains(&b),
                    ValueDef::Result(i, _) => inst_block.get(&i).is_some_and(|b| !body.contains(b)),
                    ValueDef::Unused => false,
                }
        };
        let frame = f.inst_frame[f.terminator(p).unwrap()];
        let mut picks: Vec<(Block, Inst)> = vec![];
        for &b in cfg.rpo.iter().filter(|b| body.contains(b)) {
            for &i in &f.blocks[b].insts {
                let d = &f.insts[i];
                if !d.succs.is_empty() || d.op.is_terminator() {
                    continue;
                }
                // Constants, and conversions that cost next to nothing
                // where they are and mostly feed exits and slow paths:
                // hoisted, they only lengthen live ranges across the loop.
                if matches!(
                    d.op,
                    Opcode::ConstVal(_)
                        | Opcode::ConstI32(_)
                        | Opcode::ConstF64(_)
                        | Opcode::ConstBool(_)
                        | Opcode::Box
                        | Opcode::BoxDouble
                        | Opcode::Weaken
                ) {
                    continue;
                }
                // An inlined callee's frame exists only once entered: its
                // frame-relative ops stay in it.
                if f.inst_frame[i] != frame {
                    continue;
                }
                let tys: Vec<Type> = d.args.iter().map(|&v| f.values[v].ty).collect();
                let fx = effects(&d.op, &tys, m);
                let quiet = fx.writes.is_empty()
                    && !fx.may_gc
                    && !fx.may_throw
                    && !fx.may_run_js
                    && fx.kill.is_empty();
                if !quiet
                    || fx
                        .reads
                        .iter()
                        .any(|r| writes.iter().any(|w| w.overlaps(r)))
                {
                    continue;
                }
                if !matches!(fx.flags, crate::mir::ops::FlagsEffect::Bits(b) if b == crate::mir::ops::FlagBits::NONE)
                {
                    continue;
                }
                if !d.args.iter().all(|&v| outside(v, &hoisted)) {
                    continue;
                }
                // A managed result live across the loop is rooted at each
                // of its GC points: a store and a reload there, against the
                // load hoisting saves on every iteration and the guards and
                // loads through it that can hoist after it
                // (MIR-MEMORY.md §2).
                hoisted.extend(d.results.iter().copied());
                picks.push((b, i));
            }
        }
        for (b, i) in picks {
            f.blocks[b].insts.retain(|&x| x != i);
            let pos = f.blocks[p].insts.len() - 1;
            f.blocks[p].insts.insert(pos, i);
            inst_block.insert(i, p);
            moved += 1;
        }
    }
    moved
}

/// The guards `hoist_guards` moves: a check of an operand (or of global
/// state, for `check.fuse` and `check.binding`) whose fact only a kill
/// can end, and the numeric range checks (a pure function of their
/// operand: an invariant one passes or fails every iteration alike).
pub(super) fn hoistable_guard(op: &Opcode) -> bool {
    matches!(
        op,
        Opcode::GuardUnbox(_)
            | Opcode::GuardTags(_)
            | Opcode::GuardKind(_)
            | Opcode::GuardLayout { .. }
            | Opcode::GuardScript(_)
            | Opcode::CheckFuse(_)
            | Opcode::CheckBinding(..)
            | Opcode::IntToI32
            | Opcode::GuardRange(_)
            | Opcode::F64ToIntExact
    )
}

/// Guard hoisting (§10.2): a guard in a loop whose operand is defined
/// before the loop, whose fact nothing in the loop can kill, and which
/// runs on every iteration (its block dominates the loop's latches) moves
/// to the preheader, where its failure exits at the loop header with the
/// state the loop is entered with (the preheader's params, `LoopDecl`'s
/// `entry`). A `check.fuse` or `check.binding` also needs no JS to run in
/// the loop: a fuse blows, or the global's shape changes, without a kill
/// the types record. The guard in the loop becomes a jump carrying the
/// hoisted guard's outputs. Loops entered other than by their preheader
/// (an onramp into a nested loop, a generator's resume) are left alone.
/// Returns how many guards moved.
pub fn hoist_guards(m: &Module, f: &mut Func) -> usize {
    let cfg = Cfg::new(f);
    let mut preds: BTreeMap<Block, Vec<Block>> = BTreeMap::new();
    for &b in &f.layout {
        for s in f.succs(b) {
            preds.entry(s).or_default().push(b);
        }
    }
    let mut inst_block = BTreeMap::new();
    for &b in &f.layout {
        for &i in &f.blocks[b].insts {
            inst_block.insert(i, b);
        }
    }
    let mut moved = 0;
    for li in 0..f.loops.len() {
        let l = f.loops[li].clone();
        let Some(entry) = l.entry.clone() else {
            continue;
        };
        let (h, p0) = (l.header, l.preheader);
        let Some(pt) = f.terminator(p0) else { continue };
        if f.insts[pt].op != Opcode::Jump || f.succs(p0) != vec![h] || f.inst_frame[pt] != 0 {
            continue;
        }
        let params = if entry.state.is_empty() {
            f.blocks[p0].params.clone()
        } else {
            entry.state.clone()
        };
        if entry.slots.iter().filter(|&&x| x).count() != params.len()
            || f.frame.exit_arity(entry.pc) != Some(entry.slots.len())
            || f.frame.live_elided_args(&entry.slots)
        {
            continue;
        }
        let mut body: BTreeSet<Block> = BTreeSet::new();
        body.insert(h);
        let latches: Vec<Block> = preds
            .get(&h)
            .map(|ps| {
                ps.iter()
                    .copied()
                    .filter(|&q| cfg.dominates(h, q))
                    .collect()
            })
            .unwrap_or_default();
        let mut work = latches.clone();
        while let Some(b) = work.pop() {
            if body.insert(b) {
                work.extend(
                    preds
                        .get(&b)
                        .into_iter()
                        .flatten()
                        .copied()
                        .filter(|&q| cfg.reachable(q)),
                );
            }
        }
        if latches.is_empty() || body.contains(&p0) || !body.iter().all(|&b| cfg.dominates(p0, b)) {
            continue;
        }
        // Candidates: guards of values from before the loop. Then, as a
        // greatest fixpoint, judge each on the loop as it would be with
        // every candidate hoisted (their in-loop copies jumps, so what
        // only their failures reach is gone): it must still run on every
        // iteration, and nothing left in the loop may kill its fact.
        let mut cands: Vec<(Inst, Vec<Type>)> = vec![];
        for &b in cfg.rpo.iter().filter(|b| body.contains(b)) {
            let Some(t) = f.terminator(b) else { continue };
            let d = &f.insts[t];
            if !hoistable_guard(&d.op) || d.succs.len() != 2 {
                continue;
            }
            if d.args
                .iter()
                .any(|&a| def_block(f, a, &inst_block).is_none_or(|db| body.contains(&db)))
            {
                continue;
            }
            let tys: Vec<Type> = d.args.iter().map(|&v| f.values[v].ty).collect();
            let Ok(sig) = signature(&d.op, &tys, m) else {
                continue;
            };
            cands.push((t, sig.outputs));
        }
        loop {
            let set: BTreeSet<Inst> = cands.iter().map(|(t, _)| *t).collect();
            // The loop's successors with the candidates' failures pruned.
            let succs = |b: Block| -> Vec<Block> {
                match f.terminator(b) {
                    Some(t) if set.contains(&t) => vec![f.insts[t].succs[0].block],
                    _ => f.succs(b),
                }
            };
            let reach = |skip: Option<Block>| -> BTreeSet<Block> {
                let mut seen = BTreeSet::new();
                let mut work = vec![h];
                while let Some(b) = work.pop() {
                    if Some(b) == skip || !body.contains(&b) || !seen.insert(b) {
                        continue;
                    }
                    work.extend(succs(b));
                }
                seen
            };
            let live = reach(None);
            let mut kills = vec![];
            let mut runs_js = false;
            for &b in &live {
                for &i in &f.blocks[b].insts {
                    let d = &f.insts[i];
                    let tys: Vec<Type> = d.args.iter().map(|&v| f.values[v].ty).collect();
                    let fx = effects(&d.op, &tys, m);
                    // A kill on an edge that leaves the loop (an exit)
                    // ends nothing the loop goes on with.
                    let stays = |roles: &[SuccRole]| {
                        f.succ_edges(i)
                            .any(|(r, e)| roles.contains(&r) && live.contains(&e.block))
                    };
                    let kills_here = match d.op.kill_site(&fx) {
                        KillSite::None => false,
                        KillSite::Op => true,
                        KillSite::OkEdge => stays(&[SuccRole::Ok, SuccRole::Err]),
                        KillSite::DirtyEdge => stays(&[SuccRole::OkDirty, SuccRole::Err]),
                    };
                    if kills_here {
                        kills.push(fx.kill);
                    }
                    runs_js |= fx.may_run_js;
                }
            }
            let live_latches: Vec<Block> = latches
                .iter()
                .copied()
                .filter(|q| live.contains(q))
                .collect();
            let keep: Vec<(Inst, Vec<Type>)> = cands
                .iter()
                .filter(|(t, outs)| {
                    let b = inst_block[t];
                    let d = &f.insts[*t];
                    if !live.contains(&b) {
                        return false;
                    }
                    // Every iteration passes `b`: without it, no latch.
                    let without = reach(Some(b));
                    if live_latches.iter().any(|q| without.contains(q)) {
                        return false;
                    }
                    if matches!(d.op, Opcode::CheckFuse(_) | Opcode::CheckBinding(..)) && runs_js {
                        return false;
                    }
                    !outs.iter().any(|t| kills.iter().any(|k| k.matches(t)))
                })
                .cloned()
                .collect();
            if keep.len() == cands.len() {
                break;
            }
            cands = keep;
        }
        if cands.is_empty() {
            continue;
        }
        // One exit for the lot: the loop's entry state, at its header.
        let e = f.add_block();
        let (_, dead) = f.add_inst(
            e,
            Opcode::ConstVal(crate::mir::ops::ConstVal::Dead),
            vec![],
            &[Type::VAL_TOP],
            vec![],
        );
        let mut ps = params.iter();
        let ops: Vec<Value> = entry
            .slots
            .iter()
            .map(|&x| if x { *ps.next().unwrap() } else { dead[0] })
            .collect();
        let exit = Opcode::Exit {
            pc: entry.pc,
            nargs: f.frame.formals,
            nlocals: f.frame.locals,
        };
        f.add_inst(e, exit, ops, &[], vec![]);
        for (t, outs) in cands {
            let d = f.insts[t].clone();
            let p = f.loops[li].preheader;
            let jt = f.terminator(p).unwrap();
            let jump = f.insts[jt].clone();
            f.blocks[p].insts.pop();
            let p2 = f.add_block();
            let hoisted: Vec<Value> = outs.iter().map(|&ty| f.add_param(p2, ty)).collect();
            let ok = Edge {
                block: p2,
                args: (0..outs.len())
                    .map(|k| EdgeArg::Out(u32::try_from(k).unwrap()))
                    .collect(),
            };
            let (g, _) = f.add_inst(
                p,
                d.op,
                d.args.clone(),
                &[],
                vec![
                    ok,
                    Edge {
                        block: e,
                        args: vec![],
                    },
                ],
            );
            f.inst_frame[g] = 0;
            let (j, _) = f.add_inst(p2, Opcode::Jump, vec![], &[], jump.succs.clone());
            f.inst_frame[j] = 0;
            // The guard in the loop: now a jump with the hoisted outputs.
            let b = inst_block[&t];
            let okd = d.succs[0].clone();
            let args = okd
                .args
                .iter()
                .map(|a| match *a {
                    EdgeArg::Out(k) => EdgeArg::Value(hoisted[k as usize]),
                    ref a => *a,
                })
                .collect();
            let frame = f.inst_frame[t];
            f.blocks[b].insts.pop();
            let (nj, _) = f.add_inst(
                b,
                Opcode::Jump,
                vec![],
                &[],
                vec![Edge {
                    block: okd.block,
                    args,
                }],
            );
            f.inst_frame[nj] = frame;
            f.loops[li].preheader = p2;
            if let Some(e) = f.loops[li].entry.as_mut() {
                e.state = params.clone();
            }
            moved += 1;
        }
        // The CFG changed under the loops still to visit.
        return moved;
    }
    moved
}

/// Hoist a loop's invariant read *diamonds*: a single-entry region whose
/// entry is a guard (or `getprop.data`) on values from before the loop,
/// made only of guards, `getprop.data` and quiet reads of nothing the loop
/// writes, which rejoins at one block. The typed read of a predicted
/// layout with its by-name fallback (`guard.layout` -> `load_slot` |
/// `getprop.data` -> join) is one: no single op of it can move (the guard
/// does not exit, so `hoist_guards` leaves it), but the whole computes one
/// invariant value. The region moves to the preheader, its exits (a
/// `getprop.data` that would run code, a guard with an exiting failure)
/// become the loop's entry exit as `hoist_guards`' do, and the loop's
/// entry to it jumps to the join with the hoisted values. A failure there
/// is an exit the loop might not have taken (it may not have read the
/// field at all), which is sound: baseline runs the loop. Returns how many
/// regions moved; one loop per call, since the CFG changes.
pub fn hoist_reads(m: &Module, f: &mut Func) -> usize {
    let cfg = Cfg::new(f);
    let mut preds: BTreeMap<Block, Vec<Block>> = BTreeMap::new();
    for &b in &f.layout {
        for s in f.succs(b) {
            preds.entry(s).or_default().push(b);
        }
    }
    let mut inst_block = BTreeMap::new();
    for &b in &f.layout {
        for &i in &f.blocks[b].insts {
            inst_block.insert(i, b);
        }
    }
    let quiet = |fx: &Effects| {
        fx.writes.is_empty()
            && !fx.may_gc
            && !fx.may_throw
            && !fx.may_run_js
            && fx.kill.is_empty()
            && matches!(fx.flags, crate::mir::ops::FlagsEffect::Bits(b) if b == crate::mir::ops::FlagBits::NONE)
    };
    for li in 0..f.loops.len() {
        let l = f.loops[li].clone();
        let Some(entry) = l.entry.clone() else {
            continue;
        };
        let (h, p0) = (l.header, l.preheader);
        let Some(pt) = f.terminator(p0) else { continue };
        if f.insts[pt].op != Opcode::Jump || f.succs(p0) != vec![h] || f.inst_frame[pt] != 0 {
            continue;
        }
        let params = if entry.state.is_empty() {
            f.blocks[p0].params.clone()
        } else {
            entry.state.clone()
        };
        if entry.slots.iter().filter(|&&x| x).count() != params.len()
            || f.frame.exit_arity(entry.pc) != Some(entry.slots.len())
            || f.frame.live_elided_args(&entry.slots)
        {
            continue;
        }
        let mut body: BTreeSet<Block> = BTreeSet::new();
        body.insert(h);
        let latches: Vec<Block> = preds
            .get(&h)
            .map(|ps| {
                ps.iter()
                    .copied()
                    .filter(|&q| cfg.dominates(h, q))
                    .collect()
            })
            .unwrap_or_default();
        let mut work = latches.clone();
        while let Some(b) = work.pop() {
            if body.insert(b) {
                work.extend(
                    preds
                        .get(&b)
                        .into_iter()
                        .flatten()
                        .copied()
                        .filter(|&q| cfg.reachable(q)),
                );
            }
        }
        if latches.is_empty() || body.contains(&p0) || !body.iter().all(|&b| cfg.dominates(p0, b)) {
            continue;
        }
        // What the loop writes and kills, on edges that stay in it.
        let mut writes = vec![];
        let mut kills = vec![];
        for &b in &body {
            for &i in &f.blocks[b].insts {
                let d = &f.insts[i];
                let tys: Vec<Type> = d.args.iter().map(|&v| f.values[v].ty).collect();
                let fx = effects(&d.op, &tys, m);
                if d.succs.is_empty() {
                    writes.extend(crate::mir::mem::writes_on(f, m, i, &fx, None));
                }
                for (role, e) in f.succ_edges(i) {
                    if body.contains(&e.block) {
                        writes.extend(crate::mir::mem::writes_on(f, m, i, &fx, Some(role)));
                    }
                }
                if !fx.kill.is_empty() {
                    kills.push(fx.kill);
                }
            }
        }
        let outside = |v: Value| def_block(f, v, &inst_block).is_some_and(|b| !body.contains(&b));
        // A value from before the loop up to representation: a chain of
        // `box`/`weaken`/`unbox` in the loop (which `licm` leaves) over
        // one, which moves with the region. The chain's insts, defs first.
        let chain = |v: Value| -> Option<Vec<Inst>> {
            let mut out = vec![];
            let mut work = vec![(v, 0usize)];
            while let Some((v, depth)) = work.pop() {
                if outside(v) {
                    continue;
                }
                let ValueDef::Result(i, 0) = f.values[v].def else {
                    return None;
                };
                let d = &f.insts[i];
                if depth > 8
                    || f.inst_frame[i] != 0
                    || !matches!(
                        d.op,
                        Opcode::Box
                            | Opcode::Weaken
                            | Opcode::Unbox(_)
                            | Opcode::ConstBool(_)
                            | Opcode::ConstI32(_)
                            | Opcode::ConstVal(_)
                    )
                {
                    return None;
                }
                if !out.contains(&i) {
                    out.push(i);
                }
                work.extend(d.args.iter().map(|&a| (a, depth + 1)));
            }
            out.reverse();
            Some(out)
        };
        // An exit block a region's failure may go to: outside the loop,
        // no params, ending in an exit.
        let exit_block = |b: Block| {
            !body.contains(&b)
                && f.blocks[b].params.is_empty()
                && f.terminator(b)
                    .is_some_and(|t| matches!(f.insts[t].op, Opcode::Exit { .. }))
        };
        // The ops a region may hold, with an operand rule checked later.
        let movable = |i: Inst| -> bool {
            let d = &f.insts[i];
            if f.inst_frame[i] != 0 {
                return false;
            }
            let tys: Vec<Type> = d.args.iter().map(|&v| f.values[v].ty).collect();
            match d.op {
                Opcode::Jump | Opcode::GetPropData(_) => {}
                ref op
                    if hoistable_guard(op)
                        && !matches!(op, Opcode::CheckFuse(_) | Opcode::CheckBinding(..)) => {}
                ref op if !op.is_terminator() => {
                    if matches!(op, Opcode::FrameStore(_)) {
                        return false;
                    }
                }
                _ => return false,
            }
            let fx = effects(&d.op, &tys, m);
            quiet(&fx)
                && !fx
                    .reads
                    .iter()
                    .any(|r| writes.iter().any(|w| w.overlaps(r)))
        };
        for &e in cfg.rpo.iter().filter(|b| body.contains(b)) {
            let Some(t) = f.terminator(e) else { continue };
            let d = &f.insts[t];
            let starts = matches!(d.op, Opcode::GetPropData(_))
                || hoistable_guard(&d.op)
                    && !matches!(d.op, Opcode::CheckFuse(_) | Opcode::CheckBinding(..));
            if !starts || !movable(t) {
                continue;
            }
            // The entry's operands, and what its edges pass: the op moves
            // to the preheader with its edges, so both must be there.
            let edge_vals = d
                .succs
                .iter()
                .flat_map(|s| s.args.iter())
                .filter_map(|a| match *a {
                    EdgeArg::Value(v) => Some(v),
                    _ => None,
                });
            let Some(mut lift) = d
                .args
                .iter()
                .copied()
                .chain(edge_vals)
                .map(&chain)
                .collect::<Option<Vec<_>>>()
                .map(|c| c.concat())
            else {
                continue;
            };
            // Every iteration passes it (as `hoist_guards` asks): a read
            // under a condition (`if (o) s += o.t`) may be one whose
            // receiver fails it on every entry.
            let mut seen = BTreeSet::new();
            let mut work = vec![h];
            while let Some(b) = work.pop() {
                if b == e || !body.contains(&b) || !seen.insert(b) {
                    continue;
                }
                work.extend(f.succs(b));
            }
            if latches.iter().any(|q| seen.contains(q)) {
                continue;
            }
            // The join: the nearest block every path from the entry back
            // to the header passes (paths into exits leave the loop). The
            // region: what the entry reaches before it.
            let reach_avoiding = |x: Option<Block>| -> (BTreeSet<Block>, bool) {
                let mut seen = BTreeSet::new();
                let mut back = false;
                let mut work: Vec<Block> = f.insts[t].succs.iter().map(|s| s.block).collect();
                while let Some(b) = work.pop() {
                    if Some(b) == x || !body.contains(&b) {
                        continue;
                    }
                    if b == h || b == e {
                        back = true;
                        continue;
                    }
                    if seen.insert(b) {
                        if seen.len() > 64 {
                            return (seen, true);
                        }
                        work.extend(f.succs(b));
                    }
                }
                (seen, back)
            };
            let (all, _) = reach_avoiding(None);
            let mut order: Vec<Block> = cfg
                .rpo
                .iter()
                .copied()
                .filter(|b| all.contains(b))
                .collect();
            order.truncate(16);
            let Some((join, region)) = order.into_iter().find_map(|x| {
                let (r, back) = reach_avoiding(Some(x));
                (!back).then_some((x, r))
            }) else {
                continue;
            };
            if !region
                .iter()
                .all(|&b| f.blocks[b].insts.iter().all(|&i| movable(i)))
            {
                continue;
            }
            // Its edges go to itself, the join, or exits.
            let ok = std::iter::once(t)
                .chain(region.iter().filter_map(|&b| f.terminator(b)))
                .all(|i| {
                    f.insts[i].succs.iter().all(|s| {
                        region.contains(&s.block)
                            || s.block == join
                            || exit_block(s.block) && s.args.is_empty()
                    })
                });
            // Entered only from the entry and itself.
            let single = region.iter().all(|b| {
                preds
                    .get(b)
                    .into_iter()
                    .flatten()
                    .filter(|&&q| cfg.reachable(q))
                    .all(|q| *q == e || region.contains(q))
            });
            if !ok || !single || region.iter().any(|&b| f.terminator(b).is_none()) {
                continue;
            }
            // Its operands are from before the loop or its own; its
            // values reach the rest of the loop only as the join's params,
            // which no kill in the loop may end.
            let own = |v: Value| def_block(f, v, &inst_block).is_some_and(|b| region.contains(&b));
            let mut operands_ok = true;
            for &i in region.iter().flat_map(|&b| f.blocks[b].insts.iter()) {
                let d = &f.insts[i];
                let vals = d.args.iter().copied().chain(
                    d.succs
                        .iter()
                        .flat_map(|s| s.args.iter())
                        .filter_map(|a| match *a {
                            EdgeArg::Value(v) => Some(v),
                            _ => None,
                        }),
                );
                for v in vals {
                    if own(v) {
                        continue;
                    }
                    match chain(v) {
                        Some(c) => lift.extend(c),
                        None => operands_ok = false,
                    }
                }
            }
            let mut seen_lift = BTreeSet::new();
            lift.retain(|&i| seen_lift.insert(i));
            // What the lifted chain defines lives across the loop where
            // the loop still uses it.
            let lifted_tys: Vec<Type> = lift
                .iter()
                .flat_map(|&i| f.insts[i].results.iter().map(|&v| f.values[v].ty))
                .collect();
            let join_tys: Vec<Type> = f.blocks[join]
                .params
                .iter()
                .map(|&v| f.values[v].ty)
                .collect();
            if !operands_ok
                || join_tys
                    .iter()
                    .chain(&lifted_tys)
                    .any(|ty| kills.iter().any(|k| k.matches(ty)))
            {
                continue;
            }
            // Move it: the entry's op ends the preheader, the region's
            // edges to the join go to a new preheader with its params, and
            // its exits are the loop's entry exit.
            let ex = f.add_block();
            let (_, dead) = f.add_inst(
                ex,
                Opcode::ConstVal(crate::mir::ops::ConstVal::Dead),
                vec![],
                &[Type::VAL_TOP],
                vec![],
            );
            let mut ps = params.iter();
            let ops: Vec<Value> = entry
                .slots
                .iter()
                .map(|&x| if x { *ps.next().unwrap() } else { dead[0] })
                .collect();
            let exit = Opcode::Exit {
                pc: entry.pc,
                nargs: f.frame.formals,
                nlocals: f.frame.locals,
            };
            f.add_inst(ex, exit, ops, &[], vec![]);
            let p2 = f.add_block();
            let hoisted: Vec<Value> = join_tys.iter().map(|&ty| f.add_param(p2, ty)).collect();
            let jump = f.insts[pt].clone();
            f.blocks[p0].insts.pop();
            let (nj, _) = f.add_inst(p2, Opcode::Jump, vec![], &[], jump.succs.clone());
            f.inst_frame[nj] = 0;
            let retarget = |f: &mut Func, i: Inst| {
                for s in f.insts[i].succs.iter_mut() {
                    if s.block == join {
                        s.block = p2;
                    } else if !region.contains(&s.block)
                        && s.args.is_empty()
                        && !body.contains(&s.block)
                    {
                        s.block = ex;
                    }
                }
            };
            for &b in &region {
                let rt = f.terminator(b).unwrap();
                retarget(f, rt);
            }
            for &i in &lift {
                let b = inst_block[&i];
                f.blocks[b].insts.retain(|&x| x != i);
                f.blocks[p0].insts.push(i);
            }
            let td = f.insts[t].clone();
            f.blocks[e].insts.pop();
            let (moved, _) = f.add_inst(p0, td.op, td.args.clone(), &[], td.succs.clone());
            f.inst_frame[moved] = 0;
            retarget(f, moved);
            let frame = f.inst_frame[t];
            let args = hoisted.iter().map(|&v| EdgeArg::Value(v)).collect();
            let (ej, _) = f.add_inst(
                e,
                Opcode::Jump,
                vec![],
                &[],
                vec![Edge { block: join, args }],
            );
            f.inst_frame[ej] = frame;
            f.loops[li].preheader = p2;
            if let Some(en) = f.loops[li].entry.as_mut() {
                en.state = params.clone();
            }
            return 1;
        }
    }
    0
}
