//! trunc: passes of `opt.rs`, split out unchanged.

use super::*;

/// The most a truncation-demanded chain may add up, in units of 2^31:
/// past 2^53 a double sum rounds before ToInt32 sees it, and the wrapped
/// int32 sum would no longer be its ToInt32.
pub(super) const TRUNC_UNITS_CAP: u64 = 1 << 22;

/// Truncation demand over SSA (§10.6: rewrite `.ovf` to `.wrap` under
/// ToInt32 demand). An int32 add or sub whose result only ever reaches
/// ToInt32 -- a bit op or shift operand (a shift count is masked to five
/// bits, so it too sees only low bits), another such add or sub, or a
/// block param whose own uses are all of these -- cannot have its
/// overflow observed: ToInt32 of the exact double sum is the wrapping
/// int32 sum. Such an `i32.add.ovf` becomes `i32.add.wrap` and a jump,
/// and its overflow exit goes away.
///
/// Unlike the builder's `int32_demand`, which follows the operand stack
/// within one block, this follows values through locals and block
/// params, so `h += h << 10; h ^= h >> 6` (a hash mix, where the sum
/// passes through the local) wraps too.
///
/// Exact only while the double sum is: each value carries a bound on its
/// spec magnitude in units of 2^31 (an int32 is 1, a sum the sum of its
/// operands', a param the max over its incoming values), and a chain past
/// `TRUNC_UNITS_CAP` -- any add carried around a loop -- keeps its check.
///
/// Returns how many ops it rewrote.
pub fn trunc_demand(m: &Module, f: &mut Func) -> usize {
    // Each rewrite drops an overflow exit, and an exit holds the sums live
    // at it: another round may find them demanded.
    let mut total = 0;
    loop {
        let n = trunc_demand_once(m, f);
        if n == 0 {
            if total > 0 {
                retype_ints(m, f);
            }
            return total;
        }
        total += n;
    }
}

pub(super) fn trunc_demand_once(m: &Module, f: &mut Func) -> usize {
    use crate::mir::func::InstData;
    // Reachable blocks only: a rewritten check's exit block stays in the
    // layout, unreached, with the values it would have written.
    let live = Cfg::new(f).rpo;
    let is_int = |f: &Func, v: Value| matches!(f.values[v].ty, Type::I32(_));
    let add_sub = |op: &Opcode| {
        matches!(
            op,
            Opcode::I32Ovf(ArithOp::Add | ArithOp::Sub)
                | Opcode::I32Wrap(ArithOp::Add | ArithOp::Sub)
        )
    };
    // The value an add or sub produces: a wrap's result, an ovf's `ok`
    // output param (`None` when the ok edge drops it).
    let produced = |f: &Func, d: &InstData| -> Option<Value> {
        match d.op {
            Opcode::I32Wrap(_) => d.results.first().copied(),
            Opcode::I32Ovf(_) => ok_output(f, &d.succs[0]),
            _ => None,
        }
    };
    #[derive(Clone, Copy)]
    enum Use {
        Arg(Inst),
        Param(Value),
        Full,
        // An exit's operand: it needs the exact value, which an exit can
        // recompute from a sum's operands (`recover_at_exits`).
        Exit,
    }
    let mut uses: HashMap<Value, Vec<Use>> = HashMap::new();
    for &b in &live {
        for &i in &f.blocks[b].insts {
            let d = &f.insts[i];
            let is_exit = matches!(d.op, Opcode::Exit { .. });
            for &a in &d.args {
                uses.entry(a)
                    .or_default()
                    .push(if is_exit { Use::Exit } else { Use::Arg(i) });
            }
            for e in &d.succs {
                for (k, a) in e.args.iter().enumerate() {
                    if let EdgeArg::Value(v) = a {
                        let p = f.blocks[e.block].params[k];
                        uses.entry(*v).or_default().push(Use::Param(p));
                    }
                }
            }
        }
    }
    for l in &f.loops {
        if let Some(en) = &l.entry {
            for &v in &en.state {
                uses.entry(v).or_default().push(Use::Full);
            }
        }
    }
    // Greatest fixpoint, by worklist: every int32 value is demanded until
    // a use says otherwise. A value's standing depends on the values its
    // demanded uses produce (an add's result, an edge's param), so losing
    // one re-examines the values that feed it.
    let mut feeds: HashMap<Value, Vec<Value>> = HashMap::new();
    for (&v, us) in &uses {
        for u in us {
            let dep = match *u {
                Use::Arg(i) if add_sub(&f.insts[i].op) => produced(f, &f.insts[i]),
                Use::Param(p) => Some(p),
                _ => None,
            };
            if let Some(d) = dep {
                feeds.entry(d).or_default().push(v);
            }
        }
    }
    let mut trunc: BTreeSet<Value> = f.values.keys().filter(|&v| is_int(f, v)).collect();
    let use_ok = |trunc: &BTreeSet<Value>, u: &Use| match *u {
        Use::Arg(i) => {
            let d = &f.insts[i];
            match d.op {
                Opcode::I32Bit(_) | Opcode::I32Ushr => true,
                ref op if add_sub(op) => produced(f, d).is_none_or(|r| trunc.contains(&r)),
                _ => false,
            }
        }
        Use::Param(p) => trunc.contains(&p),
        Use::Full => false,
        // Only a sum or product an exit can recompute: one an add, sub or
        // mul makes directly (not a param, whose producing iteration's
        // operands are gone).
        Use::Exit => false,
    };
    // `use_ok`, with an exit fine for a value the arithmetic makes here.
    let recoverable: BTreeSet<Value> = live
        .iter()
        .flat_map(|&b| f.blocks[b].insts.iter().copied())
        .filter(|&i| {
            matches!(
                f.insts[i].op,
                Opcode::I32Ovf(ArithOp::Add | ArithOp::Sub | ArithOp::Mul)
                    | Opcode::I32Wrap(ArithOp::Add | ArithOp::Sub | ArithOp::Mul)
            )
        })
        .filter_map(|i| produced(f, &f.insts[i]))
        .collect();
    let use_ok = |trunc: &BTreeSet<Value>, v: Value, u: &Use| match *u {
        Use::Exit => recoverable.contains(&v),
        _ => use_ok(trunc, u),
    };
    let mut work: Vec<Value> = trunc.iter().copied().collect();
    while let Some(v) = work.pop() {
        if !trunc.contains(&v) {
            continue;
        }
        let ok = uses
            .get(&v)
            .is_none_or(|us| us.iter().all(|u| use_ok(&trunc, v, u)));
        if !ok {
            trunc.remove(&v);
            if let Some(fs) = feeds.get(&v) {
                work.extend(fs.iter().copied().filter(|x| trunc.contains(x)));
            }
        }
    }
    // Magnitude bounds, by forward worklist over the adds and params. A
    // bound still growing after a few updates is carried around a loop:
    // unbounded.
    const MAX_UPDATES: u32 = 8;
    let unbounded = TRUNC_UNITS_CAP + 1;
    let mut units: HashMap<Value, u64> = HashMap::new();
    let mut updates: HashMap<Value, u32> = HashMap::new();
    let unit = |units: &HashMap<Value, u64>, f: &Func, v: Value| -> u64 {
        match units.get(&v) {
            Some(&u) => u,
            None if is_int(f, v) => 1,
            None => TRUNC_UNITS_CAP + 1,
        }
    };
    // Seeded with every add's result and int32 param; a raised bound
    // re-examines the adds and params it flows into.
    let mut work: Vec<Value> = vec![];
    for &b in &live {
        for &i in &f.blocks[b].insts {
            let d = &f.insts[i];
            if add_sub(&d.op) {
                if let Some(r) = produced(f, d) {
                    work.push(r);
                }
            }
        }
    }
    let mut params_in: HashMap<Value, Vec<Value>> = HashMap::new();
    for &b in &live {
        for &i in &f.blocks[b].insts {
            for e in &f.insts[i].succs {
                for (k, a) in e.args.iter().enumerate() {
                    if let EdgeArg::Value(v) = a {
                        let p = f.blocks[e.block].params[k];
                        if is_int(f, p) {
                            params_in.entry(p).or_default().push(*v);
                            work.push(p);
                        }
                    }
                }
            }
        }
    }
    let mut producer: HashMap<Value, Inst> = HashMap::new();
    for &b in &live {
        for &i in &f.blocks[b].insts {
            if add_sub(&f.insts[i].op) {
                if let Some(r) = produced(f, &f.insts[i]) {
                    producer.insert(r, i);
                }
            }
        }
    }
    while let Some(v) = work.pop() {
        let u = if let Some(&i) = producer.get(&v) {
            let d = &f.insts[i];
            (unit(&units, f, d.args[0]) + unit(&units, f, d.args[1])).min(unbounded)
        } else if let Some(ins) = params_in.get(&v) {
            ins.iter().map(|&x| unit(&units, f, x)).max().unwrap_or(1)
        } else {
            continue;
        };
        if units.get(&v).is_some_and(|&o| u <= o) {
            continue;
        }
        let n = updates.entry(v).or_insert(0);
        *n += 1;
        let u = if *n > MAX_UPDATES { unbounded } else { u };
        units.insert(v, u);
        if let Some(us) = uses.get(&v) {
            for use_ in us {
                match *use_ {
                    Use::Arg(i) if add_sub(&f.insts[i].op) => {
                        if let Some(r) = produced(f, &f.insts[i]) {
                            work.push(r);
                        }
                    }
                    Use::Param(p) => work.push(p),
                    _ => {}
                }
            }
        }
    }
    // Rewrite.
    let mut n = 0;
    for b in live {
        let Some(t) = f.terminator(b) else { continue };
        let d = f.insts[t].clone();
        let Opcode::I32Ovf(a @ (ArithOp::Add | ArithOp::Sub | ArithOp::Mul)) = d.op else {
            continue;
        };
        let out = ok_output(f, &d.succs[0]);
        if a == ArithOp::Mul {
            // A product only ToInt32 sees, whose operands' ranges keep it
            // an int32: its -0 (a zero product of a negative factor) is
            // 0 to every such use, which is all the check guards then.
            let (Type::I32(x), Type::I32(y)) = (f.values[d.args[0]].ty, f.values[d.args[1]].ty)
            else {
                continue;
            };
            let ps = [x.lo * y.lo, x.lo * y.hi, x.hi * y.lo, x.hi * y.hi];
            let fits = ps
                .iter()
                .all(|&p| (i64::from(i32::MIN)..=i64::from(i32::MAX)).contains(&p));
            if !fits || out.is_some_and(|r| !trunc.contains(&r)) {
                continue;
            }
        } else if let Some(r) = out {
            if !trunc.contains(&r) || unit(&units, f, r) > TRUNC_UNITS_CAP {
                continue;
            }
        }
        let op = Opcode::I32Wrap(a);
        let tys: Vec<Type> = d.args.iter().map(|&v| f.values[v].ty).collect();
        let Ok(sig) = signature(&op, &tys, m) else {
            continue;
        };
        f.blocks[b].insts.pop();
        let (_, rs) = f.add_inst(b, op, d.args.clone(), &[sig.results[0]], vec![]);
        let ok = &d.succs[0];
        let args = ok
            .args
            .iter()
            .map(|x| match x {
                EdgeArg::Out(0) => EdgeArg::Value(rs[0]),
                x => *x,
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
        n += 1;
    }
    if n > 0 {
        recover_at_exits(m, f);
    }
    n
}

/// How deep `recover_at_exits` follows wrapped operands (a deeper chain's
/// exact value could leave a double's exact range; `trunc_demand`'s unit
/// bound already keeps a chain it wraps far shallower).
pub(super) const RECOVER_DEPTH: u32 = 16;

/// The wrapped arithmetic that makes `v`, if any: an `i32.*.wrap`'s result,
/// or a param whose only way in is a jump passing one (a rewritten check's
/// `ok` param).
pub(super) fn wrap_recipe(
    f: &Func,
    v: Value,
    preds: &BTreeMap<Block, Vec<Inst>>,
) -> Option<(ArithOp, Value, Value)> {
    let w = match f.values[v].def {
        ValueDef::Param(b, k) => {
            let &[t] = preds.get(&b)?.as_slice() else {
                return None;
            };
            let d = &f.insts[t];
            if d.op != Opcode::Jump {
                return None;
            }
            match d.succs[0].args.get(k as usize)? {
                EdgeArg::Value(w) => *w,
                _ => return None,
            }
        }
        _ => v,
    };
    match f.values[w].def {
        ValueDef::Result(i, 0) => match f.insts[i].op {
            Opcode::I32Wrap(op @ (ArithOp::Add | ArithOp::Sub | ArithOp::Mul)) => {
                Some((op, f.insts[i].args[0], f.insts[i].args[1]))
            }
            _ => None,
        },
        _ => None,
    }
}

/// Exits that hold a wrapped sum or product (one `trunc_demand` unchecked
/// because only ToInt32 sees it, the exit aside) get its exact value
/// instead, recomputed in doubles from the operands just before the exit
/// (as Ion's recover instructions do at a bailout): the int32 operands are
/// exact, a wrapped operand recomputed in turn, and `trunc_demand`'s bounds
/// keep each result within a double's exact range.
pub(super) fn recover_at_exits(m: &Module, f: &mut Func) {
    let mut preds: BTreeMap<Block, Vec<Inst>> = BTreeMap::new();
    for &b in &f.layout {
        if let Some(t) = f.terminator(b) {
            for e in &f.insts[t].succs {
                preds.entry(e.block).or_default().push(t);
            }
        }
    }
    let live = Cfg::new(f).rpo;
    for b in live {
        let Some(t) = f.terminator(b) else { continue };
        if !matches!(f.insts[t].op, Opcode::Exit { .. }) {
            continue;
        }
        let args = f.insts[t].args.clone();
        if !args.iter().any(|&a| wrap_recipe(f, a, &preds).is_some()) {
            continue;
        }
        let frame = f.inst_frame[t];
        // Rebuild the block's tail: the recomputations, then the exit.
        f.blocks[b].insts.pop();
        let mut memo: BTreeMap<Value, Value> = BTreeMap::new();
        fn exact(
            m: &Module,
            f: &mut Func,
            b: Block,
            frame: u32,
            v: Value,
            depth: u32,
            preds: &BTreeMap<Block, Vec<Inst>>,
            memo: &mut BTreeMap<Value, Value>,
        ) -> Value {
            if let Some(&x) = memo.get(&v) {
                return x;
            }
            let push = |f: &mut Func, op: Opcode, args: Vec<Value>| -> Value {
                let tys: Vec<Type> = args.iter().map(|&a| f.values[a].ty).collect();
                let t = signature(&op, &tys, m)
                    .ok()
                    .and_then(|s| s.results.first().copied())
                    .unwrap_or(Type::F64_TOP);
                let (i, r) = f.add_inst(b, op, args, &[t], vec![]);
                f.inst_frame[i] = frame;
                r[0]
            };
            let x = match wrap_recipe(f, v, preds).filter(|_| depth < RECOVER_DEPTH) {
                Some((op, a, c)) => {
                    let ea = exact(m, f, b, frame, a, depth + 1, preds, memo);
                    let ec = exact(m, f, b, frame, c, depth + 1, preds, memo);
                    let fo = match op {
                        ArithOp::Add => crate::mir::ops::F64Op::Add,
                        ArithOp::Sub => crate::mir::ops::F64Op::Sub,
                        _ => crate::mir::ops::F64Op::Mul,
                    };
                    push(f, Opcode::F64Arith(fo), vec![ea, ec])
                }
                None => push(f, Opcode::I32ToF64, vec![v]),
            };
            memo.insert(v, x);
            x
        }
        let mut new_args = args.clone();
        for (k, &a) in args.iter().enumerate() {
            if wrap_recipe(f, a, &preds).is_some() {
                new_args[k] = exact(m, f, b, frame, a, 0, &preds, &mut memo);
            }
        }
        f.insts[t].args = new_args;
        f.blocks[b].insts.push(t);
    }
}
