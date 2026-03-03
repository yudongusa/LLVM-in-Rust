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

use std::collections::HashMap;
use crate::isel::{MachineFunction, MOperand, PReg, VReg};

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
                if pos < e.0 { e.0 = pos; }
                if pos + 1 > e.1 { e.1 = pos + 1; }
            }
            // Uses extend the end of the interval.
            for op in &instr.operands {
                if let MOperand::VReg(vr) = op {
                    let e = map.entry(*vr).or_insert((pos, pos + 1));
                    if pos < e.0 { e.0 = pos; }
                    if pos + 1 > e.1 { e.1 = pos + 1; }
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

// ── linear scan ────────────────────────────────────────────────────────────

/// Run linear-scan allocation over `intervals` using `allocatable` registers.
///
/// Spilled VRegs are recorded in [`RegAllocResult::spilled`].
pub fn linear_scan(
    intervals: &[LiveInterval],
    allocatable: &[PReg],
) -> RegAllocResult {
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
            // Spill: choose the active interval with the largest end point,
            // because that frees the register for the most future instructions.
            let spill_idx = active
                .iter()
                .enumerate()
                .max_by_key(|(_, (end, _, _))| end)
                .map(|(i, _)| i);

            if let Some(idx) = spill_idx {
                let (spill_end, spill_vr, spill_pr) = active[idx];
                if spill_end > interval.end {
                    // Spill the active interval; reclaim its register for current.
                    active.remove(idx);
                    result.vreg_to_preg.remove(&spill_vr); // revoke previous assignment
                    result.vreg_to_preg.insert(interval.vreg, spill_pr);
                    active.push((interval.end, interval.vreg, spill_pr));
                    active.sort_unstable_by_key(|(e, _, _)| *e);
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
            active.push((interval.end, interval.vreg, pr));
            active.sort_unstable_by_key(|(e, _, _)| *e);
        }
    }

    result
}

// ── apply allocation ───────────────────────────────────────────────────────

/// Replace `MOperand::VReg` with `MOperand::PReg` in `mf` according to
/// `result`.  Spilled VRegs are left unchanged (the caller must handle them).
pub fn apply_allocation(mf: &mut MachineFunction, result: &RegAllocResult) {
    for block in &mut mf.blocks {
        for instr in &mut block.instrs {
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
        LiveInterval { vreg: VReg(vreg), start, end }
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
        use crate::isel::{MachineFunction, MInstr, MOpcode};
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

        assert_eq!(mf.blocks[0].instrs[0].operands[0], MOperand::PReg(PReg(7)));
        assert_eq!(mf.blocks[0].instrs[1].operands[0], MOperand::PReg(PReg(3)));
    }

    #[test]
    fn compute_intervals_single_block() {
        use crate::isel::{MachineFunction, MInstr, MOpcode};
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
}
