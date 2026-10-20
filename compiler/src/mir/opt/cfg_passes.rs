//! cfg passes: passes of `opt.rs`, split out unchanged.

use super::*;

/// Jump threading: an edge into a block that computes nothing but
/// constants and weakenings and ends in a `jump`, or in a `br` whose
/// condition that edge makes a constant, goes straight to where that
/// block would send it, the block's own instructions copied along. The
/// builder's boolean diamonds (`x != null`, `!c`, a tag test's merge) then
/// branch where they test instead of materializing a bool for a later
/// branch. An edge is never threaded into a loop header, a loop's
/// preheader or a root, so loops keep their shape. Returns how many edges moved.
pub fn thread_jumps(f: &mut Func) -> usize {
    let mut fixed: BTreeSet<Block> = f.roots.iter().map(|r| r.block).collect();
    for l in &f.loops {
        fixed.insert(l.header);
        fixed.insert(l.preheader);
    }
    let copyable = |op: &Opcode| {
        matches!(
            op,
            Opcode::ConstVal(_)
                | Opcode::ConstI32(_)
                | Opcode::ConstF64(_)
                | Opcode::ConstBool(_)
                | Opcode::Weaken
        )
    };
    let const_bool = |f: &Func, v: Value| match f.values[v].def {
        ValueDef::Result(i, 0) => match f.insts[i].op {
            Opcode::ConstBool(c) => Some(c),
            _ => None,
        },
        _ => None,
    };
    // A block is skipped only where nothing it defines is used past it:
    // the edges that bypass it would leave such a use undominated.
    let mut inst_block = BTreeMap::new();
    for &b in &f.layout {
        for &i in &f.blocks[b].insts {
            inst_block.insert(i, b);
        }
    }
    let mut escapes: BTreeSet<Block> = BTreeSet::new();
    for &b in &f.layout {
        for &i in &f.blocks[b].insts {
            let d = &f.insts[i];
            let edge_vals = d.succs.iter().flat_map(|e| {
                e.args.iter().filter_map(|a| match a {
                    EdgeArg::Value(v) => Some(*v),
                    EdgeArg::Out(_) => None,
                })
            });
            for v in d.args.iter().copied().chain(edge_vals) {
                if let Some(db) = def_block(f, v, &inst_block) {
                    if db != b {
                        escapes.insert(db);
                    }
                }
            }
        }
    }
    let mut moved = 0;
    for p in f.layout.clone() {
        let Some(t) = f.terminator(p) else { continue };
        for k in 0..f.insts[t].succs.len() {
            for _ in 0..16 {
                let e = f.insts[t].succs[k].clone();
                let b = e.block;
                if b == p || fixed.contains(&b) || escapes.contains(&b) {
                    break;
                }
                let insts = f.blocks[b].insts.clone();
                let Some((&bt, body)) = insts.split_last() else {
                    break;
                };
                if !body.iter().all(|&i| copyable(&f.insts[i].op)) {
                    break;
                }
                // What each of `b`'s params and results is on this edge.
                let mut map: BTreeMap<Value, EdgeArg> = BTreeMap::new();
                for (&q, &a) in f.blocks[b].params.iter().zip(&e.args) {
                    map.insert(q, a);
                }
                let bd = f.insts[bt].clone();
                let next = match bd.op {
                    Opcode::Jump => bd.succs[0].clone(),
                    Opcode::Br => {
                        let c = match map.get(&bd.args[0]) {
                            Some(EdgeArg::Value(v)) => const_bool(f, *v),
                            Some(EdgeArg::Out(_)) => None,
                            None => const_bool(f, bd.args[0]),
                        };
                        match c {
                            Some(true) => bd.succs[0].clone(),
                            Some(false) => bd.succs[1].clone(),
                            None => break,
                        }
                    }
                    _ => break,
                };
                if next.block == b || fixed.contains(&next.block) {
                    break;
                }
                // Keep every output the edge carries (a guard's proof is
                // what later folding finds it by).
                let outs_in: BTreeSet<u32> = e
                    .args
                    .iter()
                    .filter_map(|a| match a {
                        EdgeArg::Out(n) => Some(*n),
                        EdgeArg::Value(_) => None,
                    })
                    .collect();
                let outs_on: BTreeSet<u32> = next
                    .args
                    .iter()
                    .filter_map(|a| match a {
                        EdgeArg::Value(v) => match map.get(v) {
                            Some(EdgeArg::Out(n)) => Some(*n),
                            _ => None,
                        },
                        EdgeArg::Out(_) => None,
                    })
                    .collect();
                if outs_in != outs_on {
                    break;
                }
                // Copy the block's instructions the new edge needs into
                // `p`, before its terminator, in order.
                let mut need: BTreeSet<Value> = BTreeSet::new();
                for a in &next.args {
                    if let EdgeArg::Value(v) = a {
                        need.insert(*v);
                    }
                }
                let mut ok = true;
                let mut copies: Vec<(Inst, Value)> = vec![];
                for &i in body.iter().rev() {
                    let r = f.insts[i].results[0];
                    if !need.contains(&r) {
                        continue;
                    }
                    for &a in &f.insts[i].args {
                        match map.get(&a) {
                            Some(EdgeArg::Out(_)) => ok = false,
                            _ => {
                                need.insert(a);
                            }
                        }
                    }
                    copies.push((i, r));
                }
                // A weakening copied before a fence would be live across
                // it: only before a terminator that kills nothing.
                let quiet =
                    matches!(f.insts[t].op, Opcode::Jump | Opcode::Br) || is_guard(&f.insts[t].op);
                if !ok || (!quiet && copies.iter().any(|&(i, _)| f.insts[i].op == Opcode::Weaken)) {
                    break;
                }
                copies.reverse();
                for (i, r) in copies {
                    let d = f.insts[i].clone();
                    let args: Vec<Value> = d
                        .args
                        .iter()
                        .map(|a| match map.get(a) {
                            Some(EdgeArg::Value(v)) => *v,
                            _ => *a,
                        })
                        .collect();
                    let ty = f.values[r].ty;
                    let n = f.insts.push(crate::mir::func::InstData {
                        op: d.op,
                        args,
                        results: vec![],
                        succs: vec![],
                        attach: None,
                    });
                    let nv = f.values.push(crate::mir::func::ValueData {
                        def: ValueDef::Result(n, 0),
                        ty,
                    });
                    f.insts[n].results = vec![nv];
                    let at = f.blocks[p].insts.len() - 1;
                    f.blocks[p].insts.insert(at, n);
                    map.insert(r, EdgeArg::Value(nv));
                }
                let args = next
                    .args
                    .iter()
                    .map(|a| match a {
                        EdgeArg::Value(v) => map.get(v).copied().unwrap_or(*a),
                        EdgeArg::Out(_) => unreachable!("a jump or br has no outputs"),
                    })
                    .collect();
                f.insts[t].succs[k] = Edge {
                    block: next.block,
                    args,
                };
                moved += 1;
            }
        }
    }
    moved
}

/// Replace each param of a (non-root) block that every incoming edge
/// passes the same value (or the param itself, around a loop) by that
/// value, when it is a value (not a terminator output). The value's type
/// is at most the param's, so every use stays well-typed. Not across a
/// fence edge that kills a component of the value's type: that param is
/// what weakens it (§5's fence rule). To a fixpoint: forwarding one param
/// can make another redundant.
pub fn forward_params(m: &Module, f: &mut Func) {
    forward_params_only(m, f);
    prune_dead_params(f);
}

/// Remove the params of (non-root) blocks that no instruction, edge or loop
/// entry state uses and every incoming edge passes a value (not a
/// terminator's output), with their incoming edges' args, to a fixpoint
/// (dropping an edge arg can leave the param it came from unused). A pass
/// that adds params wherever a value may be needed (`pea`'s object on every
/// block of its real part) leaves one where its incoming values differ but
/// nothing reads it, and `forward_params` keeps any param whose edges
/// disagree. Such a param on a shared throw block makes it a block with
/// params, so the validator holds its uses to the fences of every `err`
/// edge into it (§5.3). A loop's header and preheader keep theirs: the
/// preheader's are the loop's entry slots by position.
pub(super) fn prune_dead_params(f: &mut Func) {
    let mut keep: BTreeSet<Block> = f.roots.iter().map(|r| r.block).collect();
    for l in &f.loops {
        keep.insert(l.header);
        keep.insert(l.preheader);
    }
    let mut incoming: BTreeMap<Block, Vec<(Inst, usize)>> = BTreeMap::new();
    for &b in &f.layout {
        if let Some(t) = f.terminator(b) {
            for (k, e) in f.insts[t].succs.iter().enumerate() {
                incoming.entry(e.block).or_default().push((t, k));
            }
        }
    }
    loop {
        let mut used: BTreeSet<Value> = BTreeSet::new();
        for &b in &f.layout {
            for &i in &f.blocks[b].insts {
                let d = &f.insts[i];
                used.extend(d.args.iter().copied());
                for e in &d.succs {
                    for a in &e.args {
                        if let EdgeArg::Value(v) = a {
                            used.insert(*v);
                        }
                    }
                }
            }
        }
        for l in &f.loops {
            if let Some(e) = &l.entry {
                used.extend(e.state.iter().copied());
            }
        }
        let mut changed = false;
        for &b in &f.layout {
            if keep.contains(&b) {
                continue;
            }
            let params = f.blocks[b].params.clone();
            let inc = incoming.get(&b).map_or(&[][..], |v| &v[..]);
            // A terminator's output stays even unused: a later guard of
            // the same value folds into it (`fold_guards`).
            let passed = |n: usize| {
                inc.iter()
                    .all(|&(t, k)| matches!(f.insts[t].succs[k].args[n], EdgeArg::Value(_)))
            };
            let dead: Vec<usize> = (0..params.len())
                .filter(|&n| !used.contains(&params[n]) && passed(n))
                .collect();
            if dead.is_empty() {
                continue;
            }
            changed = true;
            let live = |n: &usize| !dead.contains(n);
            f.blocks[b].params = params
                .iter()
                .enumerate()
                .filter(|(n, _)| live(n))
                .map(|(_, &p)| p)
                .collect();
            for &(t, k) in incoming.get(&b).map_or(&[][..], |v| &v[..]) {
                let args = std::mem::take(&mut f.insts[t].succs[k].args);
                f.insts[t].succs[k].args = args
                    .into_iter()
                    .enumerate()
                    .filter(|(n, _)| live(n))
                    .map(|(_, a)| a)
                    .collect();
            }
            for &n in &dead {
                f.values[params[n]].def = ValueDef::Unused;
            }
            for (n, &p) in f.blocks[b].params.clone().iter().enumerate() {
                f.values[p].def = ValueDef::Param(b, n as u32);
            }
        }
        if !changed {
            break;
        }
    }
}

pub(super) fn forward_params_only(m: &Module, f: &mut Func) {
    let roots: BTreeSet<Block> = f.roots.iter().map(|r| r.block).collect();
    let mut incoming: BTreeMap<Block, Vec<(Inst, usize)>> = BTreeMap::new();
    for &b in &f.layout {
        if let Some(t) = f.terminator(b) {
            for (k, e) in f.insts[t].succs.iter().enumerate() {
                incoming.entry(e.block).or_default().push((t, k));
            }
        }
    }
    let mut subst: BTreeMap<Value, Value> = BTreeMap::new();
    let resolve = |subst: &BTreeMap<Value, Value>, mut v: Value| {
        while let Some(&w) = subst.get(&v) {
            v = w;
        }
        v
    };
    loop {
        let mut changed = false;
        for &b in &f.layout {
            if roots.contains(&b) {
                continue;
            }
            let Some(inc) = incoming.get(&b) else {
                continue;
            };
            let params = f.blocks[b].params.clone();
            let mut drop = vec![];
            for (n, &p) in params.iter().enumerate() {
                let mut only: Option<Value> = None;
                let mut ok = true;
                for &(t, k) in inc {
                    match f.insts[t].succs[k].args[n] {
                        EdgeArg::Value(v) => {
                            let v = resolve(&subst, v);
                            if v == p {
                                continue;
                            }
                            if fence_kills(m, f, t, k, v) {
                                ok = false;
                                break;
                            }
                            if only.is_some_and(|o| o != v) {
                                ok = false;
                                break;
                            }
                            only = Some(v);
                        }
                        _ => {
                            ok = false;
                            break;
                        }
                    }
                }
                if let (true, Some(v)) = (ok, only) {
                    subst.insert(p, v);
                    drop.push(n);
                }
            }
            if drop.is_empty() {
                continue;
            }
            changed = true;
            let keep = |n: &usize| !drop.contains(n);
            f.blocks[b].params = params
                .iter()
                .enumerate()
                .filter(|(n, _)| keep(n))
                .map(|(_, &p)| p)
                .collect();
            for &(t, k) in inc {
                let args = std::mem::take(&mut f.insts[t].succs[k].args);
                f.insts[t].succs[k].args = args
                    .into_iter()
                    .enumerate()
                    .filter(|(n, _)| keep(n))
                    .map(|(_, a)| a)
                    .collect();
            }
            for (n, &p) in f.blocks[b].params.clone().iter().enumerate() {
                f.values[p].def = ValueDef::Param(b, n as u32);
            }
        }
        if !changed {
            break;
        }
    }
    if subst.is_empty() {
        return;
    }
    let resolve = |mut v: Value| {
        while let Some(&w) = subst.get(&v) {
            v = w;
        }
        v
    };
    let insts: Vec<Inst> = f
        .layout
        .iter()
        .flat_map(|&b| f.blocks[b].insts.clone())
        .collect();
    for i in insts {
        let d = &mut f.insts[i];
        for a in &mut d.args {
            *a = resolve(*a);
        }
        for e in &mut d.succs {
            for a in &mut e.args {
                if let EdgeArg::Value(v) = a {
                    *v = resolve(*v);
                }
            }
        }
    }
    for l in &mut f.loops {
        if let Some(e) = l.entry.as_mut() {
            for v in &mut e.state {
                *v = resolve(*v);
            }
        }
    }
    for p in subst.keys() {
        f.values[*p].def = ValueDef::Unused;
    }
}

/// §10.4: a value only exits use (a `box` of a value no fence can weaken,
/// the global object) is made on their paths, not on the hot one: a copy
/// at the head of each exit block that uses it, and none where it was.
/// Its operands dominated the original, and so every exit block it
/// reached; a value with killable type components is left alone (its
/// live range would cross the fences the original's ended before).
pub fn sink_to_exits(f: &mut Func) -> usize {
    let is_exit = |f: &Func, b: Block| {
        f.terminator(b).is_some_and(|t| {
            matches!(
                f.insts[t].op,
                Opcode::Exit { .. } | Opcode::ExitThrow { .. } | Opcode::ExitInline { .. }
            )
        })
    };
    // Every use, by block; a use on an edge disqualifies.
    let mut use_blocks: BTreeMap<Value, BTreeSet<Block>> = BTreeMap::new();
    let mut edge_used: BTreeSet<Value> = BTreeSet::new();
    for &b in &f.layout {
        for &i in &f.blocks[b].insts {
            let d = &f.insts[i];
            for &a in &d.args {
                use_blocks.entry(a).or_default().insert(b);
            }
            for e in &d.succs {
                for a in &e.args {
                    if let EdgeArg::Value(v) = a {
                        edge_used.insert(*v);
                    }
                }
            }
        }
    }
    let mut sunk = 0;
    for b in f.layout.clone() {
        for i in f.blocks[b].insts.clone() {
            let d = f.insts[i].clone();
            let sinkable = match d.op {
                Opcode::GlobalObject => true,
                Opcode::Box => d.args.iter().all(|&a| {
                    !matches!(f.values[a].ty, Type::Raw(_))
                        && f.values[a].ty.killable_components().is_empty()
                }),
                _ => false,
            };
            let [r] = d.results[..] else { continue };
            if !sinkable || edge_used.contains(&r) {
                continue;
            }
            let Some(ub) = use_blocks.get(&r) else {
                continue;
            };
            if ub.contains(&b) || !ub.iter().all(|&u| is_exit(f, u)) {
                continue;
            }
            let ty = f.values[r].ty;
            for &u in ub.clone().iter() {
                let (j, rs) = f.add_inst(u, d.op, d.args.clone(), &[ty], vec![]);
                f.inst_frame[j] = f.inst_frame[i];
                let insts = &mut f.blocks[u].insts;
                insts.pop();
                insts.insert(0, j);
                for &k in &f.blocks[u].insts.clone() {
                    for a in f.insts[k].args.iter_mut() {
                        if *a == r {
                            *a = rs[0];
                        }
                    }
                }
                // The copy's operands are used here now.
                for &a in &d.args {
                    use_blocks.entry(a).or_default().insert(u);
                }
            }
            f.blocks[b].insts.retain(|&k| k != i);
            f.values[r].def = ValueDef::Unused;
            sunk += 1;
        }
    }
    sunk
}
