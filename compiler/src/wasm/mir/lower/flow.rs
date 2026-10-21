//! flow: methods of `Lower`, split out of `lower.rs` unchanged.

use super::*;

mod census;
mod homes;
mod liveness;
mod resume;

impl<'a> Lower<'a> {
    pub(super) fn run(&mut self) -> R<()> {
        let f = self.f;
        let reach = self.reachable();
        self.liveness(&reach);
        let order = self.rpo();
        for (i, &b) in order.iter().enumerate() {
            self.rpo_index.insert(b, i);
            for s in f.succs(b) {
                self.preds.entry(s).or_default().push(b);
            }
        }
        self.homes(&reach, &order);
        self.frame_end[0] = self.root_base + 8 * self.nslots;
        self.frame_top[0] = self.frame_end[0];
        self.inline_ks();
        self.scc_tops(&order);
        // A region in a cycle overlays only what the cycle's calls do not
        // scan: else each entry of it would dirty slots the cycle's other
        // calls then clear again, every iteration.
        for b in f.blocks.keys() {
            let Some(&top) = self.scc_top.get(&b) else {
                continue;
            };
            for &i in &f.blocks[b].insts {
                let fr = f.inst_frame[i];
                if fr != 0 {
                    let o = self.outer_frame(fr) as usize;
                    self.inline_k[o] = self.inline_k[o].max(top);
                }
            }
        }
        if self.inline {
            self.inline_layout();
        }
        // Waffle blocks with the block's own params; its managed live-ins
        // are placed when it is entered.
        for &b in &f.layout {
            if !reach.contains(&b) {
                continue;
            }
            let wb = self.body.add_block();
            for &p in &f.blocks[b].params {
                if let Some(t) = machine(&self.ty(p)) {
                    self.body.add_blockparam(wb, t);
                }
            }
            self.blocks.insert(b, wb);
        }
        self.value_tag("(entry)");
        self.entry()?;
        // In reverse postorder, so a block's dominators, which define the
        // unmanaged values it uses directly, are lowered before it.
        for b in order {
            self.cur_mblock = b;
            self.value_tag("(edges)");
            self.enter_block(b)?;
            if let Some(census) = self.block_census {
                self.value_tag("(census)");
                static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
                let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let ops: Vec<String> = f.blocks[b]
                    .insts
                    .iter()
                    .map(|&i| mir::print::mnemonic(&f.insts[i].op))
                    .collect();
                crate::diag_line!(
                    "night: mir block {id} sid#{} {b} {}",
                    self.f.script,
                    ops.join(" ")
                );
                let (k, i) = (
                    self.i32c(crate::options::MIR_BLOCK_CENSUS_KIND),
                    self.i32c(id),
                );
                self.call1(census, &[k, i], Type::I32);
                self.value_tag("(edges)");
            }
            for &inst in &f.blocks[b].insts {
                self.cold = false;
                self.cur_inst = Some(inst);
                let before = self.body.values.len();
                if self.viz_marks.is_some() && self.value_census.is_none() {
                    self.value_tag("inst");
                }
                if self.value_census.is_some() {
                    let mut tag = mir::print::mnemonic(&f.insts[inst].op);
                    if let Some(site) = f.insts[inst].attach.and_then(|a| f.attachments[a].site) {
                        tag = format!("{tag}@{site}");
                    } else if let Some(pc) = self.exit_pc(inst) {
                        let fr = f.inst_frame[inst];
                        let script = if fr == 0 {
                            f.script
                        } else {
                            f.inline_frames[fr as usize - 1].script
                        };
                        tag = format!("{tag}@{script}:{pc}");
                    }
                    self.value_tag(&tag);
                }
                self.inst(inst)?;
                let e = self
                    .opsize
                    .entry(mir::print::mnemonic(&f.insts[inst].op))
                    .or_default();
                e.0 += 1;
                e.1 += u32::try_from(self.body.values.len() - before).unwrap();
            }
        }
        if let Some((b, _)) = self.pending.iter().find(|(_, v)| !v.is_empty()) {
            return Err(format!("lowering: an edge into {b} was never placed"));
        }
        self.value_tag("(hubs)");
        // Each `exit.inline` hub continues at its site's tail.
        for (_, last, site, err, same, tails) in std::mem::take(&mut self.inline_hubs).into_values()
        {
            let targets: Vec<BlockTarget> = tails
                .iter()
                .map(|&t| BlockTarget {
                    block: t,
                    args: vec![err, same],
                })
                .collect();
            let default = targets[0].clone();
            self.body.set_terminator(
                last,
                Terminator::Select {
                    value: site,
                    targets,
                    default,
                },
            );
        }
        waffle::passes::empty_blocks::run(&mut self.body);
        self.instrument_values();
        Ok(())
    }

    pub(super) fn to(block: Block) -> BlockTarget {
        BlockTarget {
            block,
            args: vec![],
        }
    }

    /// The waffle target for MIR edge `e`, with `outs` standing for the
    /// terminator's outputs.
    pub(super) fn target(&mut self, e: &Edge, outs: &[Value]) -> R<BlockTarget> {
        if !self.blocks.contains_key(&e.block) {
            return Err(format!("lowering: {} is unreachable", e.block));
        }
        let mut args = vec![];
        let params = self.f.blocks[e.block].params.clone();
        for (a, &p) in e.args.iter().zip(&params) {
            if machine(&self.ty(p)).is_none() {
                continue;
            }
            args.push(match *a {
                EdgeArg::Value(v) => self.value_here(v)?,
                EdgeArg::Out(k) => *outs
                    .get(k as usize)
                    .ok_or_else(|| format!("lowering: no output %{k}"))?,
            });
        }
        self.edge_into(e.block, args)
    }

    /// Branch to MIR edge `e` from a fresh block, returning that block's
    /// target (for a `CondBr` arm that must carry args).
    pub(super) fn edge(&mut self, inst: mir::Inst, k: usize, outs: &[Value]) -> R<BlockTarget> {
        let e = self.f.insts[inst].succs[k].clone();
        self.target(&e, outs)
    }
}
