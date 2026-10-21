//! MIR optimization passes (MIR.md §10).
//!
//! - [`fold_guards`]: guard folding (§10.1), by type and by availability.
//! - [`forward_params`]: a param of a block with one predecessor is the
//!   value that predecessor passes.
//! - [`crate::mir::mem::mem_vn`]: heap reads numbered by the memory
//!   version they read, and store-to-load forwarding (MIR-MEMORY.md §3).
//! - [`licm`]: loop-invariant code motion of pure ops and of reads no
//!   op in the loop may write, into the loop's preheader.
//! - [`canon_loads`]: a `load_field` whose receiver proves its slot is a
//!   `load_slot`, a pure read (MIR-MEMORY.md §1.1).
//! - [`optimize`]: all, to a fixpoint.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::mir::entity::{Block, Inst, Value};
use crate::mir::func::{Edge, EdgeArg, Func, RootKind, ValueDef};
use crate::mir::module::Module;
use crate::mir::ops::{effects, signature, ArithOp, Effects, KillSite, Opcode, SuccRole};
use crate::mir::types::{is_subtype, IRange, Type};

mod cfg_passes;
pub use self::cfg_passes::{forward_params, sink_to_exits, thread_jumps};
mod folds;
pub use self::folds::{canon_loads, canon_stores, fold_guards, fold_unbox};
mod gvn;
pub use self::gvn::gvn;
mod hoist;
pub use self::hoist::{hoist_guards, hoist_reads, licm};
mod ranges;
use self::ranges::retype_ints;
pub use self::ranges::{fold_ints, narrow_ints, onramp_ranges};
mod trunc;
pub use self::trunc::trunc_demand;

/// A guard's identity: its op (with its static params) and its operand.
type GuardKey = (Opcode, Value);

/// Whether edge `k` of terminator `t` is a fence that kills a component
/// of `v`'s type.
fn fence_kills(m: &Module, f: &Func, t: Inst, k: usize, v: Value) -> bool {
    let d = &f.insts[t];
    let tys: Vec<Type> = d.args.iter().map(|&a| f.values[a].ty).collect();
    let fx = effects(&d.op, &tys, m);
    let fence_role = match d.op.kill_site(&fx) {
        KillSite::OkEdge => SuccRole::Ok,
        KillSite::DirtyEdge => SuccRole::OkDirty,
        KillSite::Op => return fx.kill.matches(&f.values[v].ty),
        KillSite::None => return false,
    };
    let role = d.op.roles().get(k).copied();
    (role == Some(fence_role) || role == Some(SuccRole::Err)) && fx.kill.matches(&f.values[v].ty)
}

/// A guard's operand up to unboxing: `unbox` is a pure function of its
/// operand, so guards on two unboxings of one value are the same guard.
pub(super) fn canon(f: &Func, v: Value) -> Value {
    match f.values[v].def {
        ValueDef::Result(i, 0) if matches!(f.insts[i].op, Opcode::Unbox(_)) => f.insts[i].args[0],
        _ => v,
    }
}

pub(super) fn is_guard(op: &Opcode) -> bool {
    matches!(
        op,
        Opcode::GuardUnbox(_)
            | Opcode::GuardTags(_)
            | Opcode::GuardKind(_)
            | Opcode::GuardLayout { .. }
            | Opcode::GuardSingleton(_)
            | Opcode::GuardScript(_)
            | Opcode::MethodLoad { .. }
            | Opcode::CheckFuse(_)
            | Opcode::CheckBinding(..)
            | Opcode::IntToI32
            | Opcode::GuardRange(_)
            | Opcode::F64ToIntExact
    )
}

/// Whether an available check `a` proves `b` where their facts' types
/// agree: a binding's checks differ in what they prove of it, not in
/// their fact (a read's fact is a write's).
fn check_implies(a: &Opcode, b: &Opcode) -> bool {
    match (a, b) {
        (Opcode::CheckBinding(x, ka), Opcode::CheckBinding(y, kb)) => x == y && ka.implies(*kb),
        _ => true,
    }
}

/// The operand a guard is keyed by: its operand up to unboxing, or, for
/// an operand-less check (`check.fuse`), a stand-in.
fn guard_arg(f: &Func, d: &crate::mir::func::InstData) -> Value {
    match d.args.first() {
        Some(&a) => canon(f, a),
        None => Value::from_u32(u32::MAX),
    }
}

/// Predecessors, reverse postorder from the roots, and immediate
/// dominators (over a virtual root above every root).
pub(crate) struct Cfg {
    pub(crate) rpo: Vec<Block>,
    /// Pre/post numbers in the dominator tree.
    dom: BTreeMap<Block, (u32, u32)>,
}

impl Cfg {
    pub(crate) fn new(f: &Func) -> Cfg {
        let mut preds: BTreeMap<Block, Vec<Block>> = BTreeMap::new();
        for &b in &f.layout {
            preds.entry(b).or_default();
            for s in f.succs(b) {
                preds.entry(s).or_default().push(b);
            }
        }
        let mut seen = BTreeSet::new();
        let mut post = vec![];
        for r in &f.roots {
            if !seen.insert(r.block) {
                continue;
            }
            let mut stack = vec![(r.block, f.succs(r.block), 0usize)];
            while let Some((b, succs, i)) = stack.last_mut() {
                if *i < succs.len() {
                    let s = succs[*i];
                    *i += 1;
                    if seen.insert(s) {
                        let ss = f.succs(s);
                        stack.push((s, ss, 0));
                    }
                } else {
                    post.push(*b);
                    stack.pop();
                }
            }
        }
        post.reverse();
        let rpo = post;
        let index: BTreeMap<Block, usize> = rpo.iter().enumerate().map(|(i, &b)| (b, i)).collect();
        let roots: BTreeSet<Block> = f.roots.iter().map(|r| r.block).collect();
        // Cooper-Harvey-Kennedy; `None` is the virtual root.
        let mut idom: BTreeMap<Block, Option<Block>> = BTreeMap::new();
        for &r in &roots {
            idom.insert(r, None);
        }
        let intersect = |idom: &BTreeMap<Block, Option<Block>>, a: Block, b: Block| {
            let (mut a, mut b) = (Some(a), Some(b));
            while a != b {
                match (a, b) {
                    (Some(x), Some(y)) => {
                        if index[&x] > index[&y] {
                            a = idom[&x];
                        } else {
                            b = idom[&y];
                        }
                    }
                    _ => return None,
                }
            }
            a
        };
        let mut changed = true;
        while changed {
            changed = false;
            for &b in &rpo {
                if roots.contains(&b) {
                    continue;
                }
                let mut new: Option<Option<Block>> = None;
                for &p in &preds[&b] {
                    if !idom.contains_key(&p) {
                        continue;
                    }
                    new = Some(match new {
                        None => Some(p),
                        Some(None) => None,
                        Some(Some(cur)) => intersect(&idom, cur, p),
                    });
                }
                if let Some(n) = new {
                    if idom.get(&b) != Some(&n) {
                        idom.insert(b, n);
                        changed = true;
                    }
                }
            }
        }
        let mut children: BTreeMap<Option<Block>, Vec<Block>> = BTreeMap::new();
        for (&b, &d) in &idom {
            children.entry(d).or_default().push(b);
        }
        let mut dom = BTreeMap::new();
        let mut n = 0u32;
        let mut pre = BTreeMap::new();
        let mut stack: Vec<(Block, bool)> = children
            .get(&None)
            .map(|c| c.iter().rev().map(|&b| (b, false)).collect())
            .unwrap_or_default();
        while let Some((b, done)) = stack.pop() {
            if done {
                dom.insert(b, (pre[&b], n));
                n += 1;
                continue;
            }
            pre.insert(b, n);
            n += 1;
            stack.push((b, true));
            if let Some(c) = children.get(&Some(b)) {
                stack.extend(c.iter().rev().map(|&c| (c, false)));
            }
        }
        Cfg { rpo, dom }
    }

    /// Whether `b` is reachable from a root.
    pub(crate) fn reachable(&self, b: Block) -> bool {
        self.dom.contains_key(&b)
    }

    pub(crate) fn dominates(&self, a: Block, b: Block) -> bool {
        match (self.dom.get(&a), self.dom.get(&b)) {
            (Some(&(ap, aq)), Some(&(bp, bq))) => ap <= bp && bq <= aq,
            _ => false,
        }
    }
}

/// Where a value is defined: its block.
pub(crate) fn def_block(f: &Func, v: Value, inst_block: &BTreeMap<Inst, Block>) -> Option<Block> {
    match f.values[v].def {
        ValueDef::Param(b, _) => Some(b),
        ValueDef::Result(i, _) => inst_block.get(&i).copied(),
        ValueDef::Unused => None,
    }
}

/// The value a guard's `ok` edge hands its successor as output 0: the
/// param receiving it.
pub(crate) fn ok_output(f: &Func, ok: &Edge) -> Option<Value> {
    let k = ok.args.iter().position(|a| *a == EdgeArg::Out(0))?;
    Some(f.blocks[ok.block].params[k])
}

/// Run the passes to a fixpoint: each folded guard leaves its `ok`
/// block's param a copy of a known value, which exposes the next guard
/// on it to folding. Returns how many guards folded.
pub fn optimize(m: &Module, f: &mut Func) -> usize {
    // Once, first: an overflow check it removes is a guard the rest
    // then need not keep in their dataflow.
    onramp_ranges(m, f);
    narrow_ints(m, f);
    let mut total = trunc_demand(m, f);
    // A loop's entry state, as values: its preheader's params now, which
    // `forward_params` may replace by the values every edge passes (a
    // preheader with one way in), and value numbering by earlier equals.
    for l in &mut f.loops {
        if let Some(e) = l.entry.as_mut() {
            if e.state.is_empty() {
                e.state = f.blocks[l.preheader].params.clone();
            }
        }
    }
    let mut promoted = false;
    let mut pea_done = false;
    loop {
        // Before the passes that value-number and hoist reads: a receiver
        // proven by an earlier round's folding makes more loads slot
        // reads.
        total += canon_loads(m, f) + canon_stores(m, f);
        // First: a loop's invariant values reach its body through params
        // until forwarded, and `hoist_guards` needs them as they are.
        forward_params(m, f);
        // Again each round: a check `fold_ints` removed was an exit
        // holding values, which may now only reach ToInt32.
        total += trunc_demand(m, f);
        let n = fold_guards(m, f)
            + crate::mir::mem::mem_vn(m, f)
            + crate::mir::mem::sroa(m, f)
            + fold_unbox(f)
            + {
                // Types only (it changes no instruction): what forwarding
                // sharpened reaches the params, for `fold_ints`.
                narrow_ints(m, f);
                fold_ints(m, f)
            }
            + gvn(m, f)
            + licm(m, f)
            + hoist_guards(m, f)
            + hoist_reads(m, f);
        forward_params(m, f);
        let threaded = thread_jumps(f);
        total += n;
        if n == 0 && threaded == 0 {
            // Then, once each, partial escape analysis and loop-carried
            // caching (neither must see its own materializations,
            // write-backs and reloads as accesses), and the passes again.
            if !pea_done {
                pea_done = true;
                let p = crate::mir::pea::pea(m, f);
                if p > 0 {
                    total += p;
                    continue;
                }
            }
            if !promoted {
                promoted = true;
                let p = crate::mir::promote::promote_all(m, f);
                if p > 0 {
                    total += p;
                    continue;
                }
            }
            total += sink_to_exits(f);
            return total;
        }
    }
}

/// Replace each value `subst` maps by its image (to a fixpoint), and
/// drop the instructions whose results are all replaced. Returns how many
/// instructions it dropped.
pub(crate) fn replace_values(f: &mut Func, subst: &BTreeMap<Value, Value>) -> usize {
    if subst.is_empty() {
        return 0;
    }
    let resolve = |mut v: Value| {
        while let Some(&w) = subst.get(&v) {
            v = w;
        }
        v
    };
    let mut dropped = 0;
    for b in f.layout.clone() {
        let before = f.blocks[b].insts.len();
        let insts = std::mem::take(&mut f.blocks[b].insts);
        f.blocks[b].insts = insts
            .into_iter()
            .filter(|&i| {
                f.insts[i].results.is_empty()
                    || !f.insts[i].results.iter().all(|r| subst.contains_key(r))
            })
            .collect();
        dropped += before - f.blocks[b].insts.len();
        for &i in &f.blocks[b].insts.clone() {
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
    }
    for l in &mut f.loops {
        if let Some(en) = l.entry.as_mut() {
            for v in &mut en.state {
                *v = resolve(*v);
            }
        }
    }
    for r in subst.keys() {
        f.values[*r].def = ValueDef::Unused;
    }
    dropped
}
