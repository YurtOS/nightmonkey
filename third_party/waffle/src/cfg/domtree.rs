/*
 * Derives from the dominator tree implementation in regalloc.rs, which is
 * licensed under the Apache Public License 2.0 with LLVM Exception. See:
 * https://github.com/bytecodealliance/regalloc.rs
 */

// NightMonkey: the upstream pass was the iterative algorithm of
//
//   A Simple, Fast Dominance Algorithm
//   Keith D. Cooper, Timothy J. Harvey, and Ken Kennedy
//   https://www.cs.rice.edu/~keith/EMBED/dom.pdf
//
// whose `merge_sets` walks the immediate-dominator chain once per
// predecessor: quadratic for a block with tens of thousands of predecessors
// (issue #29). This is Lengauer-Tarjan's semidominator computation (with
// path compression) followed by the Semi-NCA immediate-dominator step
// (Georgiadis, "Linear-Time Algorithms for Dominators and Related
// Problems"). The dominator tree is unique, so the result is identical.
//
// Everything is iterative: the DFS and the path compression can be as deep as
// the function body is long.

use crate::entity::{EntityRef, PerEntity};
use crate::ir::Block;

const NONE: usize = usize::MAX;

/// Immediate dominators of the blocks reachable from `start`: `Block::invalid()`
/// for `start` itself and for unreachable blocks.
pub fn calculate<'a, PredFn, SuccFn>(preds: PredFn, succs: SuccFn, start: Block) -> PerEntity<Block, Block>
where
    PredFn: Fn(Block) -> &'a [Block],
    SuccFn: Fn(Block) -> &'a [Block],
{
    // Depth-first search: preorder numbers, the spanning-tree parent of each
    // numbered vertex.
    let mut pre: PerEntity<Block, Option<u32>> = PerEntity::default();
    let mut vertex: Vec<Block> = vec![start];
    let mut parent: Vec<usize> = vec![NONE];
    pre[start] = Some(0);
    // (vertex number, next successor to look at)
    let mut stack: Vec<(usize, usize)> = vec![(0, 0)];
    while let Some(top) = stack.last_mut() {
        let (v, next) = *top;
        let ss = succs(vertex[v]);
        if next < ss.len() {
            top.1 += 1;
            let s = ss[next];
            if pre[s].is_none() {
                let n = vertex.len();
                pre[s] = Some(n as u32);
                vertex.push(s);
                parent.push(v);
                stack.push((n, 0));
            }
        } else {
            stack.pop();
        }
    }
    let n = vertex.len();

    // Semidominators, in reverse preorder. `ancestor`/`label` form the
    // link-eval forest: `label[v]` is the vertex of minimal semidominator on
    // the compressed path from `v` towards the root of its tree.
    let mut semi: Vec<usize> = (0..n).collect();
    let mut label: Vec<usize> = (0..n).collect();
    let mut ancestor: Vec<usize> = vec![NONE; n];
    let mut path: Vec<usize> = vec![];
    for w in (1..n).rev() {
        for &p in preds(vertex[w]) {
            // Unreachable predecessors have no number.
            let Some(v) = pre[p] else { continue };
            let u = eval(v as usize, &mut ancestor, &mut label, &semi, &mut path);
            if semi[u] < semi[w] {
                semi[w] = semi[u];
            }
        }
        ancestor[w] = parent[w];
    }

    // Semi-NCA: the immediate dominator of `w` is the nearest common ancestor
    // of `parent[w]` and `semi[w]` in the dominator tree built so far, found by
    // climbing from the parent until the number is at most `semi[w]`.
    let mut idom: Vec<usize> = vec![0; n];
    for w in 1..n {
        let mut x = parent[w];
        while x > semi[w] {
            x = idom[x];
        }
        idom[w] = x;
    }

    let mut out: PerEntity<Block, Block> = PerEntity::default();
    for w in 1..n {
        out[vertex[w]] = vertex[idom[w]];
    }
    out
}

/// Lengauer-Tarjan EVAL with iterative path compression.
fn eval(
    v: usize,
    ancestor: &mut [usize],
    label: &mut [usize],
    semi: &[usize],
    path: &mut Vec<usize>,
) -> usize {
    if ancestor[v] == NONE {
        return v;
    }
    // Collect v, anc(v), ... while the ancestor has an ancestor itself, then
    // compress from the top down (the order the recursive version unwinds).
    path.clear();
    let mut x = v;
    while ancestor[ancestor[x]] != NONE {
        path.push(x);
        x = ancestor[x];
    }
    while let Some(y) = path.pop() {
        let a = ancestor[y];
        if semi[label[a]] < semi[label[y]] {
            label[y] = label[a];
        }
        ancestor[y] = ancestor[a];
    }
    label[v]
}

pub fn dominates(idom: &PerEntity<Block, Block>, a: Block, mut b: Block) -> bool {
    loop {
        if a == b {
            return true;
        }
        if b.is_invalid() {
            return false;
        }
        b = idom[b];
    }
}
