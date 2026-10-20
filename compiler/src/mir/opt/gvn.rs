//! gvn: passes of `opt.rs`, split out unchanged.

use super::*;

/// An op `gvn` may merge with an earlier one: pure, or reading only the
/// regions its effects name, with a result that is a function of its
/// operands (and, for the frame's environment, of the frame).
pub(super) fn gvn_op(op: &Opcode) -> bool {
    use crate::mir::ops::UnboxKind;
    matches!(
        op,
        Opcode::EnvCurrent
            | Opcode::GlobalObject
            | Opcode::EnvCallee(_)
            | Opcode::EnvParent
            | Opcode::EnvLoad(_)
            | Opcode::LoadGName(_)
            | Opcode::LoadSlot(_)
            | Opcode::Unbox(
                UnboxKind::I32
                    | UnboxKind::Bool
                    | UnboxKind::Obj
                    | UnboxKind::Str
                    | UnboxKind::F64Num
            )
    )
}

/// Value numbering (MIR.md §3: facts are values, so CSE comes free) of
/// the ops `gvn_op` admits: an available-values forward dataflow over the
/// CFG, keyed by the op, its operands and (for an op reading its frame's
/// environment) its frame. An op kills the values whose reads its writes
/// overlap; an `inline.enter` kills its frame's environment (a re-entered
/// frame may hold another closure of the same script). Meet is
/// intersection. A may-GC op kills the managed values (keeping one across
/// it would root it, dearer than recomputing). A repeat whose earlier
/// value dominates it and has no
/// killable type component (so no fence can weaken it in between) is
/// removed, its uses renamed. Returns how many were removed.
pub fn gvn(m: &Module, f: &mut Func) -> usize {
    type Key = (Opcode, Vec<Value>, u32);
    let cfg = Cfg::new(f);
    let mut inst_block = BTreeMap::new();
    for &b in &f.layout {
        for &i in &f.blocks[b].insts {
            inst_block.insert(i, b);
        }
    }
    let key_of = |f: &Func, i: Inst| -> Option<Key> {
        let d = &f.insts[i];
        if !gvn_op(&d.op)
            || d.results.len() != 1
            || !f.values[d.results[0]].ty.killable_components().is_empty()
        {
            return None;
        }
        let frame = if matches!(d.op, Opcode::EnvCurrent | Opcode::EnvCallee(_)) {
            f.inst_frame[i]
        } else {
            0
        };
        Some((d.op, d.args.clone(), frame))
    };
    // What a block leaves available, and the repeats in it given `avail`.
    let transfer = |f: &Func, b: Block, avail: &HashMap<Key, Value>| {
        let mut cur = avail.clone();
        let mut repeats: Vec<(Inst, Value)> = vec![];
        for &i in &f.blocks[b].insts {
            let d = &f.insts[i];
            if let Some(k) = key_of(f, i) {
                match cur.get(&k) {
                    Some(&v) => repeats.push((i, v)),
                    None => {
                        cur.insert(k, d.results[0]);
                    }
                }
                continue;
            }
            if d.op == Opcode::InlineEnter {
                let fr = f.inst_frame[i];
                cur.retain(|(op, _, frame), _| {
                    !(*frame == fr && matches!(op, Opcode::EnvCurrent | Opcode::EnvCallee(_)))
                });
            }
            let tys: Vec<Type> = d.args.iter().map(|&a| f.values[a].ty).collect();
            let fx = effects(&d.op, &tys, m);
            if fx.may_gc {
                // A managed value kept across a GC point is rooted (a
                // store, then a reload): dearer than recomputing it.
                cur.retain(|_, v| !f.values[*v].ty.may_hold_gc_thing());
            }
            if fx.writes.is_empty() {
                continue;
            }
            cur.retain(|(op, args, _), _| {
                let atys: Vec<Type> = args.iter().map(|&a| f.values[a].ty).collect();
                let reads = effects(op, &atys, m).reads;
                !reads
                    .iter()
                    .any(|r| fx.writes.iter().any(|w| w.overlaps(r)))
            });
        }
        (cur, repeats)
    };
    let mut avail_in: BTreeMap<Block, Option<HashMap<Key, Value>>> = BTreeMap::new();
    for r in &f.roots {
        avail_in.insert(r.block, Some(HashMap::new()));
    }
    let mut changed = true;
    while changed {
        changed = false;
        for &b in &cfg.rpo {
            let Some(Some(inb)) = avail_in.get(&b).cloned() else {
                continue;
            };
            let (out, _) = transfer(f, b, &inb);
            let Some(t) = f.terminator(b) else { continue };
            for e in f.insts[t].succs.clone() {
                let s = e.block;
                let merged = match avail_in.get(&s).cloned().flatten() {
                    None if !f.roots.iter().any(|r| r.block == s) => out.clone(),
                    None => HashMap::new(),
                    Some(old) => old
                        .into_iter()
                        .filter(|(k, v)| out.get(k) == Some(v))
                        .collect(),
                };
                if avail_in.get(&s).cloned().flatten().as_ref() != Some(&merged) {
                    avail_in.insert(s, Some(merged));
                    changed = true;
                }
            }
        }
    }
    let mut subst: BTreeMap<Value, Value> = BTreeMap::new();
    let mut dead: BTreeSet<Inst> = BTreeSet::new();
    for &b in &cfg.rpo {
        let Some(Some(inb)) = avail_in.get(&b).cloned() else {
            continue;
        };
        let (_, repeats) = transfer(f, b, &inb);
        for (i, v) in repeats {
            let r = f.insts[i].results[0];
            let db = def_block(f, v, &inst_block);
            let dominates = db.is_some_and(|db| db == b || cfg.dominates(db, b));
            if dominates
                && (is_subtype(&f.values[v].ty, &f.values[r].ty)
                    || f.values[v].ty == f.values[r].ty)
            {
                subst.insert(r, v);
                dead.insert(i);
            }
        }
    }
    if dead.is_empty() {
        return 0;
    }
    let resolve = |mut v: Value| {
        while let Some(&w) = subst.get(&v) {
            v = w;
        }
        v
    };
    for &b in &f.layout.clone() {
        f.blocks[b].insts.retain(|i| !dead.contains(i));
    }
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
    for r in subst.keys() {
        f.values[*r].def = ValueDef::Unused;
    }
    dead.len()
}
