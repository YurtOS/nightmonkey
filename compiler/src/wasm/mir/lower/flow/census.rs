//! census: methods of `Lower`, split out of `flow.rs` unchanged.

use super::*;

impl<'a> Lower<'a> {
    /// The pc an instruction exits at on failure, where a successor is an
    /// exit block (the op's own bytecode pc): for `--value-census` tags.
    pub(in crate::wasm::mir::lower) fn exit_pc(&self, inst: mir::Inst) -> Option<crate::ids::Pc> {
        self.f.insts[inst].succs.iter().find_map(|e| {
            let t = self.f.terminator(e.block)?;
            match self.f.insts[t].op {
                Opcode::Exit { pc, .. }
                | Opcode::ExitThrow { pc, .. }
                | Opcode::ExitInline { pc, .. } => Some(pc),
                _ => None,
            }
        })
    }

    /// `--value-census`: the Wasm values from here on are `tag`'s.
    pub(in crate::wasm::mir::lower) fn value_tag(&mut self, tag: &str) {
        // `--viz`: an instruction's own tag marks it (the loop sets
        // `cur_inst` first); any other starts unattributed values.
        if let Some(marks) = self.viz_marks.as_mut() {
            let i = match (tag.starts_with('('), self.cur_inst) {
                (false, Some(inst)) => mir::entity::EntityRef::index(inst) as u32 + 1,
                _ => 0,
            };
            marks.push((self.body.values.len(), i));
        }
        if self.value_census.is_some() {
            self.value_tags
                .push((self.body.values.len(), tag.to_string()));
        }
    }

    /// `--value-census`: a census tick at the head of every Wasm block,
    /// and its static record of the values in it, by emitter.
    pub(in crate::wasm::mir::lower) fn instrument_values(&mut self) {
        let Some(census) = self.value_census else {
            return;
        };
        let tags = std::mem::take(&mut self.value_tags);
        let tag_of = |v: Value| -> &str {
            let i = v.index();
            match tags.partition_point(|&(s, _)| s <= i) {
                0 => "(other)",
                k => &tags[k - 1].1,
            }
        };
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let blocks: Vec<Block> = self.body.blocks.iter().collect();
        for b in blocks {
            let mut n: BTreeMap<&str, u32> = BTreeMap::new();
            for &v in &self.body.blocks[b].insts {
                *n.entry(tag_of(v)).or_default() += 1;
            }
            if n.is_empty() {
                continue;
            }
            let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let rec: Vec<String> = n.iter().map(|(t, c)| format!("{t}:{c}")).collect();
            crate::diag_line!(
                "night: mir vblock {id} sid#{} {}",
                self.f.script,
                rec.join(" ")
            );
            let len = self.body.blocks[b].insts.len();
            let saved = self.cur;
            self.cur = b;
            let (k, i) = (
                self.i32c(crate::options::MIR_VALUE_CENSUS_KIND),
                self.i32c(id),
            );
            self.call1(census, &[k, i], Type::I32);
            self.cur = saved;
            // The tick first, before the block's own instructions.
            self.body.blocks[b].insts.rotate_left(len);
        }
    }
}
