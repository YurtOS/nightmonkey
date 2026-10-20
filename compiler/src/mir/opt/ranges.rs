//! ranges: passes of `opt.rs`, split out unchanged.

use super::*;

/// Widen int32 types after `trunc_demand`: a wrapped sum may take any
/// int32 value where the checked one had a range, and every value it
/// reaches is demanded (bit ops, adds, params), so only those widen: an
/// add's result or output to its rule, a param to what its edges pass.
pub(super) fn retype_ints(m: &Module, f: &mut Func) {
    let mut changed = true;
    while changed {
        changed = false;
        for b in f.layout.clone() {
            for i in f.blocks[b].insts.clone() {
                let d = f.insts[i].clone();
                if matches!(
                    d.op,
                    Opcode::I32Ovf(_) | Opcode::I32Wrap(_) | Opcode::I32Bit(_)
                ) {
                    let tys: Vec<Type> = d.args.iter().map(|&v| f.values[v].ty).collect();
                    if let Ok(sig) = signature(&d.op, &tys, m) {
                        let got = match d.op {
                            Opcode::I32Ovf(_) => {
                                ok_output(f, &d.succs[0]).zip(sig.outputs.first().copied())
                            }
                            _ => d.results.first().copied().zip(sig.results.first().copied()),
                        };
                        if let Some((v, t)) = got {
                            if !is_subtype(&t, &f.values[v].ty) {
                                f.values[v].ty = Type::I32_TOP;
                                changed = true;
                            }
                        }
                    }
                }
                for e in &d.succs {
                    for (k, a) in e.args.iter().enumerate() {
                        if let EdgeArg::Value(v) = a {
                            let p = f.blocks[e.block].params[k];
                            let (vt, pt) = (f.values[*v].ty, f.values[p].ty);
                            if matches!((vt, pt), (Type::I32(_), Type::I32(_)))
                                && !is_subtype(&vt, &pt)
                            {
                                f.values[p].ty = Type::I32_TOP;
                                changed = true;
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Integer ops whose operands' types (sharpened since the builder, by
/// forwarding and scalar replacement) decide them (§10.6): a checked
/// `i32.*.ovf` that cannot fail is the unchecked `.wrap`, and a pure int32
/// op whose result's range is one value is that constant. Returns how
/// many it rewrote.
pub fn fold_ints(m: &Module, f: &mut Func) -> usize {
    let mut n = 0;
    for b in f.layout.clone() {
        for i in f.blocks[b].insts.clone() {
            let d = f.insts[i].clone();
            let range = |v: Value| match f.values[v].ty {
                Type::I32(r) => Some(r),
                _ => None,
            };
            match d.op {
                Opcode::I32Ovf(op) => {
                    let (Some(x), Some(y)) = (range(d.args[0]), range(d.args[1])) else {
                        continue;
                    };
                    let Some(r) = crate::mir::ops::i32_exact(op, x, y) else {
                        continue;
                    };
                    let frame = f.inst_frame[i];
                    f.blocks[b].insts.pop();
                    let (w, rs) = f.add_inst(
                        b,
                        Opcode::I32Wrap(op),
                        d.args.clone(),
                        &[Type::I32(r)],
                        vec![],
                    );
                    let ok = &d.succs[0];
                    let args = ok
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
                            block: ok.block,
                            args,
                        }],
                    );
                    f.inst_frame[w] = frame;
                    f.inst_frame[j] = frame;
                    n += 1;
                }
                Opcode::I32Wrap(_) | Opcode::I32Bit(_) => {
                    let r = d.results[0];
                    // A result declared wider than its rule gives: the
                    // rule's (sharper) type.
                    let tys: Vec<Type> = d.args.iter().map(|&v| f.values[v].ty).collect();
                    let Ok(sig) = signature(&d.op, &tys, m) else {
                        continue;
                    };
                    let Type::I32(rr) = sig.results[0] else {
                        continue;
                    };
                    if rr.lo == rr.hi {
                        f.insts[i].op = Opcode::ConstI32(i32::try_from(rr.lo).unwrap());
                        f.insts[i].args.clear();
                        f.values[r].ty = Type::I32(rr);
                        n += 1;
                    } else if is_subtype(&Type::I32(rr), &f.values[r].ty)
                        && Type::I32(rr) != f.values[r].ty
                    {
                        f.values[r].ty = Type::I32(rr);
                    }
                }
                _ => {}
            }
        }
    }
    n
}

/// How many times a value's range may grow before it widens to its
/// declared type (a loop-carried count, `i + 1`, grows every round).
pub(super) const RANGE_WIDEN_AFTER: u32 = 3;

/// The ops whose int32 result or output `int_ranges` recomputes from its
/// operands' ranges.
pub(super) fn ranged_op(op: &Opcode) -> bool {
    matches!(
        op,
        Opcode::I32Ovf(_)
            | Opcode::I32Wrap(_)
            | Opcode::I32Bit(_)
            | Opcode::ConstI32(_)
            | Opcode::GuardRange(_)
    )
}

/// The int32 ranges of a function's values, from the ops' rules and, for
/// block params, the hull of what their edges pass (§10.6): an optimistic
/// fixpoint in reverse postorder, where an edge not reached yet passes
/// nothing and a value whose range keeps growing widens to its declared
/// type. Edges from `skip` blocks do not count. Values with no entry keep
/// their declared type.
pub(super) fn int_ranges(m: &Module, f: &Func, skip: &BTreeSet<Block>) -> HashMap<Value, IRange> {
    let cfg = Cfg::new(f);
    let mut cur: HashMap<Value, IRange> = HashMap::new();
    let mut grown: HashMap<Value, u32> = HashMap::new();
    let declared = |v: Value| match f.values[v].ty {
        Type::I32(r) => Some(r),
        _ => None,
    };
    let range_of = |cur: &HashMap<Value, IRange>, v: Value| -> Option<IRange> {
        match f.values[v].def {
            ValueDef::Param(..) => cur.get(&v).copied(),
            ValueDef::Result(i, _) if ranged_op(&f.insts[i].op) => cur.get(&v).copied(),
            _ => declared(v),
        }
    };
    let mut changed = true;
    while changed {
        changed = false;
        let mut set = |cur: &mut HashMap<Value, IRange>, v: Value, r: IRange| {
            let Some(d) = declared(v) else { return };
            let r = IRange::new(r.lo.max(d.lo), r.hi.min(d.hi));
            let next = match cur.get(&v) {
                None => r,
                Some(old) if old.contains(&r) => return,
                Some(old) => {
                    let n = grown.entry(v).or_insert(0);
                    *n += 1;
                    if *n > RANGE_WIDEN_AFTER {
                        d
                    } else {
                        old.hull(&r)
                    }
                }
            };
            if cur.get(&v) != Some(&next) {
                cur.insert(v, next);
                changed = true;
            }
        };
        for &b in &cfg.rpo {
            for &i in &f.blocks[b].insts {
                let d = &f.insts[i];
                let arg_tys = |cur: &HashMap<Value, IRange>| -> Option<Vec<Type>> {
                    d.args
                        .iter()
                        .map(|&a| match f.values[a].ty {
                            Type::I32(_) => range_of(cur, a).map(Type::I32),
                            t => Some(t),
                        })
                        .collect()
                };
                let sig = if ranged_op(&d.op) {
                    arg_tys(&cur).and_then(|t| signature(&d.op, &t, m).ok())
                } else {
                    None
                };
                if let Some(s) = &sig {
                    for (&r, t) in d.results.iter().zip(&s.results) {
                        if let Type::I32(x) = t {
                            set(&mut cur, r, *x);
                        }
                    }
                }
                if skip.contains(&b) {
                    continue;
                }
                for e in &d.succs {
                    for (k, a) in e.args.iter().enumerate() {
                        let p = f.blocks[e.block].params[k];
                        let Some(pd) = declared(p) else { continue };
                        let r = match *a {
                            EdgeArg::Value(v) => match f.values[v].ty {
                                Type::I32(_) => range_of(&cur, v),
                                _ => Some(pd),
                            },
                            EdgeArg::Out(o) => match &sig {
                                Some(s) => match s.outputs.get(o as usize) {
                                    Some(Type::I32(x)) => Some(*x),
                                    _ => Some(pd),
                                },
                                // Not ranged: the op's own output type.
                                None => Some(pd),
                            },
                        };
                        if let Some(r) = r {
                            set(&mut cur, p, r);
                        }
                    }
                }
            }
        }
    }
    cur
}

/// Narrow int32 values to the ranges `int_ranges` finds (block params
/// especially, whose declared type is the builder's `i32`). Returns how
/// many types it narrowed.
pub fn narrow_ints(m: &Module, f: &mut Func) -> usize {
    let r = int_ranges(m, f, &BTreeSet::new());
    let mut n = 0;
    for (v, x) in r {
        if let Type::I32(d) = f.values[v].ty {
            if d.contains(&x) && d != x {
                f.values[v].ty = Type::I32(x);
                n += 1;
            }
        }
    }
    n
}

/// An onramp's values come from the baseline frame, so a loop param an
/// onramp feeds has no range however narrow the loop's entry path makes
/// it. Where the param's other edges prove a range the onramp's value
/// lacks, the onramp guards its value into that range (exiting at the
/// loop header, as its other guards do), so the param keeps it. The
/// ranges hold of the program, not of a speculation, so the guard never
/// fails in practice. Returns how many guards it added.
pub fn onramp_ranges(m: &Module, f: &mut Func) -> usize {
    let preheaders: BTreeSet<Block> = f.loops.iter().map(|l| l.preheader).collect();
    // Each onramp's chain (its blocks until the preheader), its jump into
    // the preheader, and its exit block.
    let mut chains: Vec<(BTreeSet<Block>, Inst, Block, Block)> = vec![];
    for r in f.roots.clone() {
        if !matches!(r.kind, RootKind::Onramp(_)) {
            continue;
        }
        let mut blocks = BTreeSet::new();
        let (mut b, mut exit) = (r.block, None);
        let jump = loop {
            blocks.insert(b);
            let Some(t) = f.terminator(b) else { break None };
            let d = &f.insts[t];
            if d.op == Opcode::Jump {
                break preheaders.contains(&d.succs[0].block).then_some(t);
            }
            if !is_guard(&d.op) || d.succs.len() != 2 {
                break None;
            }
            let fb = d.succs[1].block;
            if f.terminator(fb)
                .is_some_and(|x| f.insts[x].op.exit_shape().is_some())
                && f.blocks[fb].params.is_empty()
            {
                exit = Some(fb);
            }
            b = d.succs[0].block;
        };
        if let (Some(j), Some(x)) = (jump, exit) {
            chains.push((blocks, j, b, x));
        }
    }
    if chains.is_empty() {
        return 0;
    }
    let skip: BTreeSet<Block> = chains.iter().flat_map(|c| c.0.iter().copied()).collect();
    let others = int_ranges(m, f, &skip);
    let mut added = 0;
    for (_, j, b, exit) in chains {
        let pre = f.insts[j].succs[0].block;
        let mut args = f.insts[j].succs[0].args.clone();
        // The onramp enters mid-loop: a value must be in the range of the
        // header param its preheader param becomes (the loop-carried hull,
        // back edges included), not in the entry path's own range.
        let Some(pt) = f.terminator(pre) else {
            continue;
        };
        let pd = &f.insts[pt];
        if pd.op != Opcode::Jump {
            continue;
        }
        let header = pd.succs[0].block;
        let mut want: Vec<(usize, Value, IRange)> = vec![];
        for (k, a) in args.iter().enumerate() {
            let EdgeArg::Value(v) = *a else { continue };
            let p = f.blocks[pre].params[k];
            let into: Vec<Value> = pd.succs[0]
                .args
                .iter()
                .zip(&f.blocks[header].params)
                .filter(|(x, _)| **x == EdgeArg::Value(p))
                .map(|(_, &h)| h)
                .collect();
            let need = match into.as_slice() {
                // Not carried by the header: the entry range is all there is.
                [] => others.get(&p).copied(),
                [h] => others.get(h).copied(),
                _ => None,
            };
            let (Type::I32(have), Some(need)) = (f.values[v].ty, need) else {
                continue;
            };
            if have.contains(&need) && have != need {
                want.push((k, v, need));
            }
        }
        if want.is_empty() {
            continue;
        }
        let frame = f.inst_frame[j];
        // Replace the jump by a chain of guards ending in it.
        let popped = f.blocks[b].insts.pop();
        debug_assert_eq!(popped, Some(j));
        let mut at = b;
        for (k, v, need) in want {
            let nb = f.add_block();
            let out = f.add_param(nb, Type::I32(need));
            let (g, _) = f.add_inst(
                at,
                Opcode::GuardRange(need),
                vec![v],
                &[],
                vec![
                    Edge {
                        block: nb,
                        args: vec![EdgeArg::Out(0)],
                    },
                    Edge {
                        block: exit,
                        args: vec![],
                    },
                ],
            );
            f.inst_frame[g] = frame;
            args[k] = EdgeArg::Value(out);
            at = nb;
            added += 1;
        }
        let (nj, _) = f.add_inst(
            at,
            Opcode::Jump,
            vec![],
            &[],
            vec![Edge { block: pre, args }],
        );
        f.inst_frame[nj] = frame;
    }
    added
}
