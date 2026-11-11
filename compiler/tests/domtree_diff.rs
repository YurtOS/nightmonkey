//! The vendored waffle's dominator pass (third_party/waffle/NIGHTMONKEY.md)
//! against a copy of the iterative Cooper-Harvey-Kennedy pass it replaced.
//! The dominator tree is unique, so the two must agree exactly: the same
//! immediate dominator for every reachable block, `invalid` for the start
//! block and for unreachable blocks.

// Index loops read better than enumerate() for building graphs by block number.
#![allow(clippy::needless_range_loop)]

use waffle::cfg::domtree;
use waffle::entity::{EntityRef, PerEntity};
use waffle::Block;

fn b(i: usize) -> Block {
    Block::new(i)
}

/// Postorder of the blocks reachable from `start` (iterative DFS).
fn postorder(succs: &[Vec<Block>], start: Block) -> Vec<Block> {
    let mut seen = vec![false; succs.len()];
    let mut out = vec![];
    let mut stack = vec![(start, 0usize)];
    seen[start.index()] = true;
    while let Some(top) = stack.last_mut() {
        let (v, i) = *top;
        if i < succs[v.index()].len() {
            top.1 += 1;
            let s = succs[v.index()][i];
            if !seen[s.index()] {
                seen[s.index()] = true;
                stack.push((s, 0));
            }
        } else {
            out.push(v);
            stack.pop();
        }
    }
    out
}

// ---- the replaced pass, verbatim apart from the signature ----

fn merge_sets(
    idom: &PerEntity<Block, Block>,
    block_to_rpo: &PerEntity<Block, Option<u32>>,
    mut node1: Block,
    mut node2: Block,
) -> Block {
    while node1 != node2 {
        if node1.is_invalid() || node2.is_invalid() {
            return Block::invalid();
        }
        let rpo1 = block_to_rpo[node1].unwrap();
        let rpo2 = block_to_rpo[node2].unwrap();
        if rpo1 > rpo2 {
            node1 = idom[node1];
        } else if rpo2 > rpo1 {
            node2 = idom[node2];
        }
    }
    node1
}

fn chk(preds: &[Vec<Block>], post_ord: &[Block], start: Block) -> PerEntity<Block, Block> {
    let mut block_to_rpo: PerEntity<Block, Option<u32>> = PerEntity::default();
    for (i, rpo_block) in post_ord.iter().rev().enumerate() {
        block_to_rpo[*rpo_block] = Some(i as u32);
    }
    let mut idom: PerEntity<Block, Block> = PerEntity::default();
    idom[start] = start;
    let mut changed = true;
    while changed {
        changed = false;
        for &node in post_ord.iter().rev() {
            let rponum = block_to_rpo[node].unwrap();
            let mut parent = Block::invalid();
            for &pred in preds[node.index()].iter() {
                let Some(pred_rpo) = block_to_rpo[pred] else {
                    continue;
                };
                if pred_rpo < rponum {
                    parent = pred;
                    break;
                }
            }
            if parent != Block::invalid() {
                for &pred in preds[node.index()].iter() {
                    if pred == parent {
                        continue;
                    }
                    if idom[pred] == Block::invalid() {
                        continue;
                    }
                    parent = merge_sets(&idom, &block_to_rpo, parent, pred);
                }
            }
            if parent != Block::invalid() && parent != idom[node] {
                idom[node] = parent;
                changed = true;
            }
        }
    }
    idom[start] = Block::invalid();
    idom
}

/// Compare on a CFG given as successor lists (block 0 is the entry).
fn check(succs: &[Vec<Block>]) {
    let n = succs.len();
    let mut preds: Vec<Vec<Block>> = vec![vec![]; n];
    for (i, ss) in succs.iter().enumerate() {
        for s in ss {
            preds[s.index()].push(b(i));
        }
    }
    let start = b(0);
    let post = postorder(succs, start);
    let want = chk(&preds, &post, start);
    let got = domtree::calculate(
        |blk| &preds[blk.index()][..],
        |blk| &succs[blk.index()][..],
        start,
    );
    for i in 0..n {
        assert_eq!(got[b(i)], want[b(i)], "idom of block {i} in {succs:?}");
    }
}

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

#[test]
fn small_shapes() {
    // single block; self-loop; diamond; loop; irreducible; unreachable block
    check(&[vec![]]);
    check(&[vec![b(0)]]);
    check(&[vec![b(1), b(2)], vec![b(3)], vec![b(3)], vec![]]);
    check(&[vec![b(1)], vec![b(2)], vec![b(1), b(3)], vec![]]);
    check(&[vec![b(1), b(2)], vec![b(2)], vec![b(1)]]);
    check(&[vec![b(1)], vec![], vec![b(1)]]);
    // duplicate edges
    check(&[vec![b(1), b(1)], vec![b(2), b(2)], vec![]]);
}

#[test]
fn random_cfgs_agree() {
    let mut rng = Lcg(0x9e3779b97f4a7c15);
    for _ in 0..4000 {
        let n = 1 + rng.below(40);
        let mut succs: Vec<Vec<Block>> = vec![vec![]; n];
        for s in succs.iter_mut() {
            for _ in 0..rng.below(4) {
                s.push(b(rng.below(n)));
            }
        }
        check(&succs);
    }
}

#[test]
fn random_sparse_cfgs_agree() {
    // Mostly-forward graphs with a few back edges: the shape of real code.
    let mut rng = Lcg(42);
    for _ in 0..2000 {
        let n = 2 + rng.below(120);
        let mut succs: Vec<Vec<Block>> = vec![vec![]; n];
        for i in 0..n {
            if i + 1 < n {
                succs[i].push(b(i + 1));
            }
            if rng.below(3) == 0 {
                succs[i].push(b(rng.below(n)));
            }
            if rng.below(4) == 0 && i + 2 < n {
                succs[i].push(b(i + 2 + rng.below(n - i - 2)));
            }
        }
        check(&succs);
    }
}

/// The shape of regress-155081: a chain of blocks that each also branch to one
/// shared sink (a catch block with a predecessor per call). Quadratic in the
/// replaced pass; here it must agree with it at a size where the oracle is
/// still quick, and finish at the real size in no time.
#[test]
fn chain_into_one_sink_agrees() {
    let n = 1500;
    let sink = n;
    let mut succs: Vec<Vec<Block>> = vec![vec![]; n + 1];
    for i in 0..n {
        if i + 1 < n {
            succs[i].push(b(i + 1));
        }
        succs[i].push(b(sink));
    }
    check(&succs);
}

#[test]
fn chain_into_one_sink_is_fast() {
    let n = 100_000;
    let sink = n;
    let mut succs: Vec<Vec<Block>> = vec![vec![]; n + 1];
    let mut preds: Vec<Vec<Block>> = vec![vec![]; n + 1];
    for i in 0..n {
        if i + 1 < n {
            succs[i].push(b(i + 1));
            preds[i + 1].push(b(i));
        }
        succs[i].push(b(sink));
        preds[sink].push(b(i));
    }
    let t = std::time::Instant::now();
    let idom = domtree::calculate(
        |blk| &preds[blk.index()][..],
        |blk| &succs[blk.index()][..],
        b(0),
    );
    let took = t.elapsed();
    // Every call block is dominated by the one before; the sink only by the first.
    assert_eq!(idom[b(sink)], b(0));
    assert_eq!(idom[b(n - 1)], b(n - 2));
    assert_eq!(idom[b(0)], Block::invalid());
    // The replaced pass took minutes at 17,500; this is milliseconds.
    assert!(
        took.as_secs() < 5,
        "dominators of 100k blocks took {took:?}"
    );
}
