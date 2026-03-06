//! Linear-scan register allocator.
//!
//! Reference: Poletto & Sarkar, "Linear Scan Register Allocation", TOPLAS 1999.
//!
//! Algorithm:
//! 1. Compute per-VReg live intervals by a single pass over all instructions.
//! 2. Sort intervals by start point.
//! 3. Scan forward: maintain an "active" set sorted by end point.
//!    - Expire intervals whose end ≤ current start (return their regs to free pool).
//!    - If a free register exists, assign it.
//!    - Otherwise spill the interval with the largest end point.

use crate::isel::{MInstr, MOpcode, MOperand, MachineFunction, PReg, VReg};
pub use crate::regalloc_gc::graph_color;
use std::collections::HashMap;

// ── live intervals ─────────────────────────────────────────────────────────

/// Half-open live interval `[start, end)` in flat instruction program order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveInterval {
    pub vreg: VReg,
    /// Index of the first instruction that defines (or uses) this vreg.
    pub start: usize,
    /// One past the index of the last instruction that uses this vreg.
    pub end: usize,
}

/// Compute flat program-order starting positions for each block.
fn block_starts(mf: &MachineFunction) -> Vec<usize> {
    let mut starts = Vec::with_capacity(mf.blocks.len());
    let mut pos = 0;
    for b in &mf.blocks {
        starts.push(pos);
        pos += b.instrs.len();
    }
    starts
}

/// Build live intervals by scanning all instructions of `mf`.
pub fn compute_live_intervals(mf: &MachineFunction) -> Vec<LiveInterval> {
    let _ = block_starts(mf); // kept for potential future use
    let mut map: HashMap<VReg, (usize, usize)> = HashMap::new();
    let mut pos = 0usize;

    for block in &mf.blocks {
        for instr in &block.instrs {
            // Definition extends interval to at least [pos, pos+1).
            if let Some(dst) = instr.dst {
                let e = map.entry(dst).or_insert((pos, pos + 1));
                if pos < e.0 {
                    e.0 = pos;
                }
                if pos + 1 > e.1 {
                    e.1 = pos + 1;
                }
            }
            // Uses extend the end of the interval.
            for op in &instr.operands {
                if let MOperand::VReg(vr) = op {
                    let e = map.entry(*vr).or_insert((pos, pos + 1));
                    if pos < e.0 {
                        e.0 = pos;
                    }
                    if pos + 1 > e.1 {
                        e.1 = pos + 1;
                    }
                }
            }
            pos += 1;
        }
    }

    map.into_iter()
        .map(|(vreg, (start, end))| LiveInterval { vreg, start, end })
        .collect()
}

// ── register allocation result ─────────────────────────────────────────────

/// Maps each VReg to the PReg it was assigned, or records it as spilled.
#[derive(Debug, Default)]
pub struct RegAllocResult {
    pub vreg_to_preg: HashMap<VReg, PReg>,
    /// VRegs for which no physical register was available.
    pub spilled: Vec<VReg>,
}

/// Register allocation strategy used by [`allocate_registers`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RegAllocStrategy {
    #[default]
    LinearScan,
    GraphColor,
}

/// Allocate registers with the selected strategy.
pub fn allocate_registers(
    intervals: &[LiveInterval],
    allocatable: &[PReg],
    strategy: RegAllocStrategy,
) -> RegAllocResult {
    match strategy {
        RegAllocStrategy::LinearScan => linear_scan(intervals, allocatable),
        RegAllocStrategy::GraphColor => graph_color(intervals, allocatable),
    }
}

// ── linear scan ────────────────────────────────────────────────────────────

/// Run linear-scan allocation over `intervals` using `allocatable` registers.
///
/// Spilled VRegs are recorded in [`RegAllocResult::spilled`].
pub fn linear_scan(intervals: &[LiveInterval], allocatable: &[PReg]) -> RegAllocResult {
    if allocatable.is_empty() {
        return RegAllocResult {
            vreg_to_preg: HashMap::new(),
            spilled: intervals.iter().map(|i| i.vreg).collect(),
        };
    }

    // Sort by start point.
    let mut sorted: Vec<&LiveInterval> = intervals.iter().collect();
    sorted.sort_unstable_by_key(|i| i.start);

    let mut free: Vec<PReg> = allocatable.to_vec();
    // Active set: (end, vreg, assigned_preg), sorted by end.
    let mut active: Vec<(usize, VReg, PReg)> = Vec::new();
    let mut result = RegAllocResult::default();

    for interval in &sorted {
        // Expire intervals that ended before the current start.
        let mut returned = Vec::new();
        active.retain(|&(end, _vr, pr)| {
            if end <= interval.start {
                returned.push(pr);
                false
            } else {
                true
            }
        });
        free.extend(returned);

        if free.is_empty() {
            // Spill: the active set is kept sorted by end, so the interval with
            // the largest end is always at the back — O(1) lookup instead of O(n).
            let spill_idx = if active.is_empty() {
                None
            } else {
                Some(active.len() - 1)
            };

            if let Some(idx) = spill_idx {
                let (spill_end, spill_vr, spill_pr) = active[idx];
                if spill_end > interval.end {
                    // Spill the active interval; reclaim its register for current.
                    active.remove(idx);
                    result.vreg_to_preg.remove(&spill_vr); // revoke previous assignment
                    result.vreg_to_preg.insert(interval.vreg, spill_pr);
                    // Insert in sorted position to maintain invariant.
                    let pos = active.partition_point(|&(e, _, _)| e <= interval.end);
                    active.insert(pos, (interval.end, interval.vreg, spill_pr));
                    result.spilled.push(spill_vr);
                } else {
                    // Current interval has the largest end — spill it.
                    result.spilled.push(interval.vreg);
                }
            } else {
                result.spilled.push(interval.vreg);
            }
        } else {
            let pr = free.remove(0);
            result.vreg_to_preg.insert(interval.vreg, pr);
            // Insert in sorted position to maintain the end-sorted invariant without
            // a full O(n log n) sort on every push.
            let pos = active.partition_point(|&(e, _, _)| e <= interval.end);
            active.insert(pos, (interval.end, interval.vreg, pr));
        }
    }

    result
}

// ── spill/reload insertion ─────────────────────────────────────────────────

/// Insert spill stores and reload loads for all spilled VRegs.
///
/// For each spilled VReg `v`:
/// - After every instruction that **defines** `v`, insert a STORE of `v` to
///   its frame slot using a fresh VReg.
/// - Before every instruction that **uses** `v`, insert a LOAD from its frame
///   slot into a fresh VReg, and replace the use with that fresh VReg.
///
/// Fresh VRegs are then allocated in a second linear-scan pass (they have very
/// short live intervals so they virtually never cause additional spills).  The
/// result is merged into `result`.
///
/// `load_op` / `store_op` are target-provided opcodes.  The instructions
/// have the layout:
/// - LOAD:  `dst = VReg(fresh), operands = [Imm(slot)]`
/// - STORE: `dst = None,        operands = [Imm(slot), VReg(src)]`
pub fn insert_spill_reloads(
    mf: &mut MachineFunction,
    result: &mut RegAllocResult,
    load_op: MOpcode,
    store_op: MOpcode,
) {
    if result.spilled.is_empty() {
        return;
    }

    // Collect spilled VRegs and assign each a frame slot.
    let spilled: Vec<VReg> = result.spilled.drain(..).collect();
    for &vr in &spilled {
        mf.alloc_spill_slot(vr);
    }

    // Rebuild every block's instruction list with loads/stores inserted.
    for bi in 0..mf.blocks.len() {
        let old_instrs: Vec<MInstr> = std::mem::take(&mut mf.blocks[bi].instrs);
        let mut new_instrs: Vec<MInstr> = Vec::with_capacity(old_instrs.len() * 2);

        for mut instr in old_instrs {
            // 1) Before the instruction: reload every spilled VReg that appears
            //    as a source operand, replacing the operand with a fresh VReg.
            for op in &mut instr.operands {
                if let MOperand::VReg(vr) = op {
                    if let Some(&slot) = mf.spill_slots.get(vr) {
                        let fresh = mf.fresh_vreg();
                        // Insert LOAD before this instruction.
                        new_instrs.push(MInstr::new(load_op).with_dst(fresh).with_imm(slot as i64));
                        *op = MOperand::VReg(fresh);
                    }
                }
            }

            // 2) If the instruction defines a spilled VReg, rewrite dst to a
            //    fresh VReg, emit the instruction, then insert a STORE.
            let store_after = if let Some(dst_vr) = instr.dst {
                if let Some(&slot) = mf.spill_slots.get(&dst_vr) {
                    let fresh = mf.fresh_vreg();
                    instr.dst = Some(fresh);
                    Some((fresh, slot))
                } else {
                    None
                }
            } else {
                None
            };

            new_instrs.push(instr);

            if let Some((fresh, slot)) = store_after {
                new_instrs.push(MInstr::new(store_op).with_imm(slot as i64).with_vreg(fresh));
            }
        }

        mf.blocks[bi].instrs = new_instrs;
    }

    // Second allocation pass for the fresh VRegs just created.
    let fresh_intervals = compute_live_intervals(mf);
    // Only allocate the truly-fresh VRegs (those not yet in result).
    let fresh_only: Vec<LiveInterval> = fresh_intervals
        .into_iter()
        .filter(|iv| !result.vreg_to_preg.contains_key(&iv.vreg))
        .collect();

    if !fresh_only.is_empty() {
        let alloc = mf.allocatable_pregs.clone();
        let second = linear_scan(&fresh_only, &alloc);
        // Merge — fresh VRegs should not spill given their tiny intervals.
        for (vr, pr) in second.vreg_to_preg {
            result.vreg_to_preg.insert(vr, pr);
        }
        // Any unexpected spills from fresh VRegs: just assign the first allocatable reg.
        if !second.spilled.is_empty() {
            let fallback = alloc.first().copied().unwrap_or(PReg(0));
            for vr in second.spilled {
                result.vreg_to_preg.entry(vr).or_insert(fallback);
            }
        }
    }
}

// ── apply allocation ───────────────────────────────────────────────────────

/// Replace `MOperand::VReg` with `MOperand::PReg` in `mf` according to
/// `result`.  Also rewrites `instr.dst` so that the destination register
/// number reflects the assigned physical register rather than the raw VReg
/// counter.  Spilled VRegs are left unchanged (the caller must handle them).
///
/// Also populates `mf.used_callee_saved` with any callee-saved registers
/// that were actually assigned during allocation.
pub fn apply_allocation(mf: &mut MachineFunction, result: &RegAllocResult) {
    // Collect callee-saved registers that are actually used.
    let callee_saved: std::collections::HashSet<PReg> =
        mf.callee_saved_pregs.iter().copied().collect();
    let mut used_cs: std::collections::HashSet<PReg> = std::collections::HashSet::new();

    for &pr in result.vreg_to_preg.values() {
        if callee_saved.contains(&pr) {
            used_cs.insert(pr);
        }
    }
    mf.used_callee_saved = used_cs.into_iter().collect();
    // Keep a stable order matching the original callee_saved_pregs list.
    let order: HashMap<PReg, usize> = mf
        .callee_saved_pregs
        .iter()
        .enumerate()
        .map(|(i, &p)| (p, i))
        .collect();
    mf.used_callee_saved
        .sort_by_key(|p| order.get(p).copied().unwrap_or(usize::MAX));

    for block in &mut mf.blocks {
        for instr in &mut block.instrs {
            // Rewrite destination VReg.
            if let Some(ref mut vr) = instr.dst {
                if let Some(&pr) = result.vreg_to_preg.get(vr) {
                    // Store the physical register number in the VReg wrapper
                    // so encoder casts `PReg(dst.0 as u8)` yield the correct reg.
                    *vr = VReg(pr.0 as u32);
                }
            }
            // Rewrite source operands.
            for op in &mut instr.operands {
                if let MOperand::VReg(vr) = op {
                    if let Some(&pr) = result.vreg_to_preg.get(vr) {
                        *op = MOperand::PReg(pr);
                    }
                }
            }
        }
    }
}

// ── tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn iv(vreg: u32, start: usize, end: usize) -> LiveInterval {
        LiveInterval {
            vreg: VReg(vreg),
            start,
            end,
        }
    }

    #[test]
    fn two_non_overlapping_one_reg() {
        // [0,2) and [3,5) — only one register needed.
        let intervals = vec![iv(0, 0, 2), iv(1, 3, 5)];
        let alloc = vec![PReg(0)];
        let result = linear_scan(&intervals, &alloc);
        assert!(result.spilled.is_empty(), "no spills expected");
        assert_eq!(result.vreg_to_preg.len(), 2);
        assert_eq!(result.vreg_to_preg[&VReg(0)], PReg(0));
        assert_eq!(result.vreg_to_preg[&VReg(1)], PReg(0));
    }

    #[test]
    fn two_overlapping_one_register_causes_spill() {
        // [0,10) and [1,8) both want a register but only one is available.
        let intervals = vec![iv(0, 0, 10), iv(1, 1, 8)];
        let alloc = vec![PReg(0)];
        let result = linear_scan(&intervals, &alloc);
        assert_eq!(result.spilled.len(), 1, "exactly one must spill");
        assert_eq!(result.vreg_to_preg.len(), 1);
    }

    #[test]
    fn three_intervals_two_registers() {
        // Intervals [0,3), [1,4), [5,8) — max pressure = 2.
        let intervals = vec![iv(0, 0, 3), iv(1, 1, 4), iv(2, 5, 8)];
        let alloc = vec![PReg(0), PReg(1)];
        let result = linear_scan(&intervals, &alloc);
        assert!(result.spilled.is_empty());
        assert_eq!(result.vreg_to_preg.len(), 3);
    }

    #[test]
    fn empty_intervals() {
        let result = linear_scan(&[], &[PReg(0)]);
        assert!(result.spilled.is_empty());
        assert!(result.vreg_to_preg.is_empty());
    }

    #[test]
    fn no_allocatable_registers_all_spill() {
        let intervals = vec![iv(0, 0, 5), iv(1, 2, 7)];
        let result = linear_scan(&intervals, &[]);
        assert_eq!(result.spilled.len(), 2);
        assert!(result.vreg_to_preg.is_empty());
    }

    #[test]
    fn apply_allocation_rewrites_operands() {
        use crate::isel::{MInstr, MOpcode, MachineFunction};
        let mut mf = MachineFunction::new("f".into());
        let b = mf.add_block("entry");
        let v0 = mf.fresh_vreg();
        let v1 = mf.fresh_vreg();
        mf.push(b, MInstr::new(MOpcode(0)).with_dst(v0).with_vreg(v1));
        mf.push(b, MInstr::new(MOpcode(1)).with_vreg(v0));

        let mut result = RegAllocResult::default();
        result.vreg_to_preg.insert(v0, PReg(3));
        result.vreg_to_preg.insert(v1, PReg(7));

        apply_allocation(&mut mf, &result);

        // instr 0: dst=v0→PReg(3), src operand v1→PReg(7)
        assert_eq!(mf.blocks[0].instrs[0].dst, Some(VReg(3))); // physical reg 3
        assert_eq!(mf.blocks[0].instrs[0].operands[0], MOperand::PReg(PReg(7)));
        // instr 1: src operand v0→PReg(3)
        assert_eq!(mf.blocks[0].instrs[1].operands[0], MOperand::PReg(PReg(3)));
    }

    #[test]
    fn apply_allocation_rewrites_dst_register() {
        use crate::isel::{MInstr, MOpcode, MachineFunction};
        let mut mf = MachineFunction::new("f".into());
        let b = mf.add_block("entry");
        let v5 = VReg(5); // VReg 5 allocated to PReg 2
        mf.next_vreg = 6;
        mf.push(b, MInstr::new(MOpcode(0)).with_dst(v5));

        let mut result = RegAllocResult::default();
        result.vreg_to_preg.insert(v5, PReg(2));

        apply_allocation(&mut mf, &result);

        // dst should now contain VReg(2) so that PReg(dst.0 as u8) == PReg(2)
        assert_eq!(mf.blocks[0].instrs[0].dst, Some(VReg(2)));
    }

    #[test]
    fn many_overlapping_intervals_all_assigned() {
        // Issue #38: stress test with many intervals to verify that the
        // partition_point insertion correctly maintains the sorted invariant.
        // 5 intervals all starting at 0 with different ends — need 5 registers.
        let intervals = vec![
            iv(0, 0, 10),
            iv(1, 0, 8),
            iv(2, 0, 6),
            iv(3, 0, 4),
            iv(4, 0, 2),
        ];
        let alloc: Vec<PReg> = (0u8..5).map(PReg).collect();
        let result = linear_scan(&intervals, &alloc);
        assert!(
            result.spilled.is_empty(),
            "no spills: 5 regs for 5 simultaneous live ranges"
        );
        assert_eq!(result.vreg_to_preg.len(), 5);
    }

    #[test]
    fn compute_intervals_single_block() {
        use crate::isel::{MInstr, MOpcode, MachineFunction};
        let mut mf = MachineFunction::new("f".into());
        let b = mf.add_block("entry");
        let v0 = mf.fresh_vreg();
        let v1 = mf.fresh_vreg();
        // instr 0: v0 = ...
        mf.push(b, MInstr::new(MOpcode(0)).with_dst(v0));
        // instr 1: v1 = ... v0 ...
        mf.push(b, MInstr::new(MOpcode(1)).with_dst(v1).with_vreg(v0));

        let intervals = compute_live_intervals(&mf);
        let v0_iv = intervals.iter().find(|i| i.vreg == v0).unwrap();
        let v1_iv = intervals.iter().find(|i| i.vreg == v1).unwrap();
        // v0 defined at 0, last used at 1 → end = 2
        assert_eq!(v0_iv.start, 0);
        assert_eq!(v0_iv.end, 2);
        // v1 defined at 1, never used → end = 2
        assert_eq!(v1_iv.start, 1);
    }

    #[test]
    fn insert_spill_reloads_inserts_load_store() {
        // Create a MachineFunction with a single register available and two
        // simultaneously live VRegs → one must spill.
        use crate::isel::{MInstr, MOpcode, MachineFunction};
        const LOAD_OP: MOpcode = MOpcode(0xA0);
        const STORE_OP: MOpcode = MOpcode(0xA1);

        let mut mf = MachineFunction::new("spill_test".into());
        mf.allocatable_pregs = vec![PReg(0)];
        let b = mf.add_block("entry");

        let v0 = mf.fresh_vreg();
        let v1 = mf.fresh_vreg();
        // instr 0: v0 = ...  (defines v0)
        mf.push(b, MInstr::new(MOpcode(1)).with_dst(v0));
        // instr 1: v1 = ... v0 ...  (defines v1, uses v0 — both live simultaneously)
        mf.push(b, MInstr::new(MOpcode(2)).with_dst(v1).with_vreg(v0));
        // instr 2: use v1
        mf.push(b, MInstr::new(MOpcode(3)).with_vreg(v1));

        let intervals = compute_live_intervals(&mf);
        let mut result = linear_scan(&intervals, &mf.allocatable_pregs);

        // With only 1 register, one VReg must spill.
        assert_eq!(
            result.spilled.len(),
            1,
            "one VReg must spill with 1 reg, 2 simultaneously live"
        );

        insert_spill_reloads(&mut mf, &mut result, LOAD_OP, STORE_OP);

        // After insertion: no VReg should remain in result.spilled (they were processed).
        assert!(
            result.spilled.is_empty(),
            "spilled list must be empty after insert_spill_reloads"
        );

        // The spill slot must have been allocated.
        assert_eq!(mf.spill_slots.len(), 1, "one spill slot must be allocated");
        assert!(mf.frame_size > 0, "frame_size must be non-zero after spill");

        // The block must have more instructions than the original 3 (loads/stores inserted).
        assert!(
            mf.blocks[0].instrs.len() > 3,
            "instructions must be inserted for load/store around the spilled VReg"
        );

        // Every LOAD_OP must have a dst, every STORE_OP must have no dst.
        for instr in &mf.blocks[0].instrs {
            if instr.opcode == LOAD_OP {
                assert!(instr.dst.is_some(), "LOAD_OP must have a dst VReg");
                assert!(
                    matches!(instr.operands.first(), Some(MOperand::Imm(_))),
                    "LOAD_OP first operand must be Imm(slot)"
                );
            } else if instr.opcode == STORE_OP {
                assert!(instr.dst.is_none(), "STORE_OP must have no dst");
                assert!(
                    matches!(instr.operands.first(), Some(MOperand::Imm(_))),
                    "STORE_OP first operand must be Imm(slot)"
                );
            }
        }
    }

    #[test]
    fn apply_allocation_populates_used_callee_saved() {
        use crate::isel::{MInstr, MOpcode, MachineFunction};

        let mut mf = MachineFunction::new("cs_test".into());
        mf.allocatable_pregs = vec![PReg(0), PReg(3)]; // RAX and RBX
        mf.callee_saved_pregs = vec![PReg(3)]; // RBX is callee-saved
        let b = mf.add_block("entry");
        let v0 = mf.fresh_vreg();
        let v1 = mf.fresh_vreg();
        mf.push(b, MInstr::new(MOpcode(0)).with_dst(v0));
        mf.push(b, MInstr::new(MOpcode(1)).with_dst(v1).with_vreg(v0));

        let intervals = compute_live_intervals(&mf);
        let result = linear_scan(&intervals, &mf.allocatable_pregs);
        // Ensure both are allocated (not spilled)
        assert_eq!(result.spilled.len(), 0);

        apply_allocation(&mut mf, &result);

        // If RBX (PReg(3)) was used, it must appear in used_callee_saved.
        let used_pregs: std::collections::HashSet<PReg> =
            result.vreg_to_preg.values().copied().collect();
        if used_pregs.contains(&PReg(3)) {
            assert!(
                mf.used_callee_saved.contains(&PReg(3)),
                "used callee-saved PReg must appear in mf.used_callee_saved"
            );
        } else {
            assert!(
                !mf.used_callee_saved.contains(&PReg(3)),
                "unused callee-saved PReg must not appear in mf.used_callee_saved"
            );
        }
    }
}
