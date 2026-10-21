//! folds: passes of `opt.rs`, split out unchanged.

use super::*;

/// `load_field` through a receiver whose type proves the field's slot
/// (`ops::slot_of`) lowers to one load and a branch to `ok_clean`; its
/// `ok_dirty` and `err` edges are dead. It becomes `load_slot` and a jump
/// to the clean successor with its value, which the value-numbering and
/// hoisting passes treat as a pure read (MIR-MEMORY.md §1.1). Returns how
/// many it rewrote.
pub fn canon_loads(m: &Module, f: &mut Func) -> usize {
    let mut n = 0;
    for b in f.layout.clone() {
        let Some(t) = f.terminator(b) else { continue };
        let Opcode::LoadField(name) = f.insts[t].op else {
            continue;
        };
        let d = f.insts[t].clone();
        let Some(o) = f.values[d.args[0]].ty.obj_info().copied() else {
            continue;
        };
        if crate::mir::ops::slot_of(&o, name, m).is_none() {
            continue;
        }
        let tys: Vec<Type> = d.args.iter().map(|&v| f.values[v].ty).collect();
        let Ok(sig) = signature(&Opcode::LoadSlot(name), &tys, m) else {
            continue;
        };
        let frame = f.inst_frame[t];
        f.blocks[b].insts.pop();
        // The frame stores before it stay: each is a local's own assignment
        // (`frame_set`), which a slot access does not make dead.
        let (l, rs) = f.add_inst(
            b,
            Opcode::LoadSlot(name),
            d.args.clone(),
            &sig.results,
            vec![],
        );
        let clean = &d.succs[0];
        let args = clean
            .args
            .iter()
            .map(|a| match a {
                EdgeArg::Out(0) => EdgeArg::Value(rs[0]),
                a => *a,
            })
            .collect();
        let (j, _) = f.add_inst(
            b,
            Opcode::Jump,
            vec![],
            &[],
            vec![Edge {
                block: clean.block,
                args,
            }],
        );
        f.inst_frame[l] = frame;
        f.inst_frame[j] = frame;
        n += 1;
    }
    n
}

/// `store_field` of a value conforming to its field's claim through a
/// receiver whose type proves the slot and TYPES cannot demote: its dirty
/// and err edges are dead. It becomes `store_slot` and a jump to the clean
/// successor, a plain write of its field (as `canon_loads` for reads).
/// Returns how many it rewrote.
pub fn canon_stores(m: &Module, f: &mut Func) -> usize {
    let mut n = 0;
    for b in f.layout.clone() {
        let Some(t) = f.terminator(b) else { continue };
        let Opcode::StoreField(name) = f.insts[t].op else {
            continue;
        };
        let d = f.insts[t].clone();
        let tys: Vec<Type> = d.args.iter().map(|&v| f.values[v].ty).collect();
        if signature(&Opcode::StoreSlot(name), &tys, m).is_err() {
            continue;
        }
        let frame = f.inst_frame[t];
        f.blocks[b].insts.pop();
        // The frame stores before it stay: each is a local's own assignment
        // (`frame_set`), which a slot access does not make dead.
        let (s, _) = f.add_inst(b, Opcode::StoreSlot(name), d.args.clone(), &[], vec![]);
        let (j, _) = f.add_inst(b, Opcode::Jump, vec![], &[], vec![d.succs[0].clone()]);
        f.inst_frame[s] = frame;
        f.inst_frame[j] = frame;
        n += 1;
    }
    n
}

/// `unbox` of a `box` (through `weaken`s) is the boxed value itself,
/// where its type is the unbox's result or stronger and has no killable
/// component (MIR-MEMORY.md §3.4):
/// what forwarding and scalar replacement leave where a stored field was
/// boxed and a load of it unboxed. Returns how many it removed.
pub fn fold_unbox(f: &mut Func) -> usize {
    let mut subst: BTreeMap<Value, Value> = BTreeMap::new();
    for &b in &f.layout {
        for &i in &f.blocks[b].insts {
            let d = &f.insts[i];
            if !matches!(d.op, Opcode::Unbox(_)) {
                continue;
            }
            let mut a = d.args[0];
            for _ in 0..8 {
                match f.values[a].def {
                    ValueDef::Result(j, 0) if f.insts[j].op == Opcode::Weaken => {
                        a = f.insts[j].args[0]
                    }
                    _ => break,
                }
            }
            let ValueDef::Result(j, 0) = f.values[a].def else {
                continue;
            };
            if f.insts[j].op != Opcode::Box {
                continue;
            }
            let x = f.insts[j].args[0];
            let (xt, rt) = (f.values[x].ty, f.values[d.results[0]].ty);
            // Not a value with a killable component (a layout claim): a
            // fence between the box and the unbox may have ended it, and
            // boxing it is how the builder drops it across one.
            if (xt == rt || is_subtype(&xt, &rt)) && xt.killable_components().is_empty() {
                subst.insert(d.results[0], x);
            }
        }
    }
    replace_values(f, &subst)
}

/// Guard folding (§10.1). A guard is redundant when:
/// - **by type:** its operand's type already proves it; or
/// - **by availability:** the same guard on the same operand succeeded on
///   every path to it, with no fence since that kills its output's type,
///   and that guard's output dominates it.
///
/// Either way it becomes a `jump` to its `ok` successor carrying the
/// known value (unboxed first when the guard would unbox). Its `fail`
/// edge goes away. Returns how many guards folded. The validator checks
/// the result.
pub fn fold_guards(m: &Module, f: &mut Func) -> usize {
    let cfg = Cfg::new(f);
    let mut inst_block = BTreeMap::new();
    for &b in &f.layout {
        for &i in &f.blocks[b].insts {
            inst_block.insert(i, b);
        }
    }
    // Available guards on block entry, by forward dataflow. `None` is
    // "not yet computed" (top); roots start empty.
    let mut avail_in: BTreeMap<Block, Option<HashMap<GuardKey, Value>>> = BTreeMap::new();
    for r in &f.roots {
        avail_in.insert(r.block, Some(HashMap::new()));
    }
    let edge_out = |f: &Func, b: Block, avail: &HashMap<GuardKey, Value>| {
        // What each successor edge of `b` hands on.
        let mut out: Vec<(Block, HashMap<GuardKey, Value>)> = vec![];
        let mut cur = avail.clone();
        let insts = &f.blocks[b].insts;
        for (idx, &i) in insts.iter().enumerate() {
            let d = &f.insts[i];
            let tys: Vec<Type> = d.args.iter().map(|&v| f.values[v].ty).collect();
            let fx = effects(&d.op, &tys, m);
            let site = d.op.kill_site(&fx);
            if site == KillSite::Op {
                cur.retain(|_, v| !fx.kill.matches(&f.values[*v].ty));
            }
            // A fuse can blow wherever JS runs, and blowing one bumps no
            // epoch: its check is not kept across such an op, clean edge
            // or not.
            if fx.may_run_js {
                cur.retain(|(op, _), _| {
                    !matches!(op, Opcode::CheckFuse(_) | Opcode::CheckBinding(..))
                });
            }
            // A method's holder property may change by any write, script or
            // not (a data store to the holder runs none).
            if fx.may_run_js || !fx.writes.is_empty() || !fx.kill.is_empty() {
                cur.retain(|(op, _), _| !matches!(op, Opcode::MethodLoad { .. }));
            }
            if idx + 1 != insts.len() {
                continue;
            }
            for (role, e) in f.succ_edges(i) {
                let mut a = cur.clone();
                let fence = matches!(
                    (site, role),
                    (KillSite::OkEdge, SuccRole::Ok | SuccRole::Err)
                        | (KillSite::DirtyEdge, SuccRole::OkDirty | SuccRole::Err)
                );
                if fence {
                    a.retain(|_, v| !fx.kill.matches(&f.values[*v].ty));
                }
                if role == SuccRole::Ok && is_guard(&d.op) {
                    if let Some(v) = ok_output(f, e) {
                        a.insert((d.op, guard_arg(f, d)), v);
                    }
                }
                out.push((e.block, a));
            }
        }
        out
    };
    let mut changed = true;
    while changed {
        changed = false;
        for &b in &cfg.rpo {
            let Some(Some(inb)) = avail_in.get(&b).cloned() else {
                continue;
            };
            for (s, a) in edge_out(f, b, &inb) {
                let merged = match avail_in.get(&s).cloned().flatten() {
                    None if !f.roots.iter().any(|r| r.block == s) => a,
                    None => HashMap::new(),
                    Some(old) => old
                        .into_iter()
                        .filter(|(k, v)| a.get(k) == Some(v))
                        .collect(),
                };
                if avail_in.get(&s).cloned().flatten().as_ref() != Some(&merged) {
                    avail_in.insert(s, Some(merged));
                    changed = true;
                }
            }
        }
    }
    // Rewrite.
    let mut folded = 0;
    for &b in &cfg.rpo.clone() {
        let Some(t) = f.terminator(b) else { continue };
        let d = f.insts[t].clone();
        if !is_guard(&d.op) {
            continue;
        }
        let x = d.args.first().copied();
        let ok = d.succs[0].clone();
        let Some(outp) = ok_output(f, &ok) else {
            continue;
        };
        let want = f.values[outp].ty;
        // By type: the operand's own type proves the guard.
        let by_type: Option<(Option<Opcode>, Type)> = match x.map(|x| (x, f.values[x].ty)) {
            None => None,
            Some((_, xt)) => match signature(&d.op, &[xt], m) {
                Ok(sig) => {
                    let out = sig.outputs[0];
                    match d.op {
                        Opcode::GuardUnbox(k) => {
                            let unbox = signature(&Opcode::Unbox(k), &[xt], m).ok();
                            unbox.map(|s| (Some(Opcode::Unbox(k)), s.results[0]))
                        }
                        _ if is_subtype(&xt, &out) || xt == out => Some((None, xt)),
                        _ => None,
                    }
                }
                Err(_) => None,
            },
        };
        let from_type = by_type.is_some() && x.is_some();
        let known: Option<(Option<Opcode>, Value, Type)> = match (by_type, x) {
            (Some((op, t)), Some(x)) => Some((op, x, t)),
            _ => {
                // A guard ends its block, so what reaches it is the
                // block's entry set.
                // The same guard, or one of its kind on the same value
                // whose output implies this one's (a layout within the
                // range guarded here).
                let avail = avail_in.get(&b).cloned().flatten().unwrap_or_default();
                let cx = guard_arg(f, &d);
                let mut hits: Vec<Value> = avail
                    .iter()
                    .filter(|((op, y), v1)| {
                        *y == cx
                            && (*op == d.op
                                || (std::mem::discriminant(op) == std::mem::discriminant(&d.op)
                                    && is_subtype(&f.values[**v1].ty, &want)
                                    && check_implies(op, &d.op)))
                    })
                    .map(|(_, &v1)| v1)
                    .collect();
                hits.sort();
                // (Not this guard's own output, which a loop can bring
                // back to its block.)
                hits.into_iter().find_map(|v1| {
                    let db = def_block(f, v1, &inst_block);
                    (v1 != outp && db.is_some_and(|db| cfg.dominates(db, b)))
                        .then(|| (None, v1, f.values[v1].ty))
                })
            }
        };
        let Some((unbox, v, vt)) = known else {
            continue;
        };
        if !(is_subtype(&vt, &want)) {
            continue;
        }
        // Replace the guard with (an unbox and) a jump.
        f.blocks[b].insts.pop();
        let v = match (unbox, x) {
            (Some(op), Some(x)) => {
                let (_, rs) = f.add_inst(b, op, vec![x], &[vt], vec![]);
                rs[0]
            }
            _ => v,
        };
        // Another guard's stronger output (a layout with `types` for one
        // without) goes on as this guard's type: an op's rule need not be
        // monotone in its operands' (a `store_field` through a `types`
        // receiver demands a conforming value).
        let v = if !from_type && matches!(want, Type::Obj(_)) && f.values[v].ty != want {
            let (_, rs) = f.add_inst(b, Opcode::Weaken, vec![v], &[want], vec![]);
            rs[0]
        } else {
            v
        };
        let args = ok
            .args
            .iter()
            .map(|a| match a {
                EdgeArg::Out(0) => EdgeArg::Value(v),
                a => *a,
            })
            .collect();
        f.add_inst(
            b,
            Opcode::Jump,
            vec![],
            &[],
            vec![Edge {
                block: ok.block,
                args,
            }],
        );
        folded += 1;
    }
    folded
}
