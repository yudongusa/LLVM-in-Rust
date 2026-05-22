//! Instruction list scheduling.
//!
//! Reorders machine instructions within a basic block to hide latency
//! and improve instruction-level parallelism (ILP).
//!
//! The implementation uses a **critical-path list scheduler**:
//! 1. Build a dependency DAG (RAW / WAR / WAW / Mem / Ctrl edges).
//! 2. Compute the critical-path length for every node (longest weighted path
//!    to a sink in the DAG).
//! 3. Greedily schedule: maintain a ready set (in-degree == 0), always pick
//!    the instruction with the highest critical-path priority.

use crate::isel::{MInstr, MOpcode, MOperand, MachineBlock, PReg, VReg};
use std::collections::BinaryHeap;

// ── dependency kinds ───────────────────────────────────────────────────────

/// Dependency edge between two instructions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepKind {
    /// Read-After-Write: instruction B reads a register written by A.
    Raw,
    /// Write-After-Read: instruction B writes a register read by A.
    War,
    /// Write-After-Write: both instructions write the same register.
    Waw,
    /// Memory dependency (conservative: any store→load, load→store, store→store).
    Mem,
    /// Control dependency: terminator depends on all prior instructions.
    Ctrl,
}

/// A directed dependency edge from instruction `from` to instruction `to`.
#[derive(Debug, Clone)]
pub struct DepEdge {
    /// Index of the dependent instruction (successor in the DAG).
    pub to: usize,
    /// Kind of dependency.
    pub kind: DepKind,
    /// Latency in cycles associated with this edge.
    pub latency: u32,
}

// ── helpers: instruction read/write sets ─────────────────────────────────

/// Sentinel encoding: we unify VReg and PReg into a single u64 key for cheap
/// comparison.  VRegs occupy the low 32 bits with bit 32 clear; PReg values
/// are placed in bits 0:7 with bit 33 set so they never alias VReg keys.
#[inline]
fn vreg_key(v: VReg) -> u64 {
    v.0 as u64
}

#[inline]
fn preg_key(p: PReg) -> u64 {
    (1u64 << 32) | (p.0 as u64)
}

/// Collect the set of register keys **written** by `instr`.
fn writes(instr: &MInstr) -> Vec<u64> {
    let mut out = Vec::new();
    if let Some(dst) = instr.dst {
        out.push(vreg_key(dst));
    }
    // Clobbered physical registers are also written.
    for &p in &instr.clobbers {
        out.push(preg_key(p));
    }
    // MOV_PR: the first operand is a PReg destination (no dst field).
    // We detect this heuristically: if operands[0] is a PReg and dst is None.
    if instr.dst.is_none() {
        if let Some(MOperand::PReg(p)) = instr.operands.first() {
            out.push(preg_key(*p));
        }
    }
    out
}

/// Collect the set of register keys **read** by `instr`.
fn reads(instr: &MInstr) -> Vec<u64> {
    let mut out = Vec::new();
    for op in &instr.operands {
        match op {
            MOperand::VReg(v) => out.push(vreg_key(*v)),
            MOperand::PReg(p) => out.push(preg_key(*p)),
            _ => {}
        }
    }
    // phys_uses are also reads (e.g. argument registers at call sites).
    for &p in &instr.phys_uses {
        out.push(preg_key(p));
    }
    out
}

// ── memory-op classification ───────────────────────────────────────────────

/// Returns `true` if the opcode represents a memory access (load or store).
///
/// The function uses opcode ranges that match the x86 instruction constants
/// in `llvm-target-x86/src/instructions.rs`, but the scheduler is used as a
/// target-independent module so we also check a few common patterns from
/// other backends.  When in doubt we err on the side of conservatism.
fn is_memory_op(opcode: MOpcode) -> bool {
    // x86-64 MOpcode constants (see llvm-target-x86/src/instructions.rs):
    //   MOV_LOAD_MR   = 0x72  (spill reload)
    //   MOV_STORE_RM  = 0x73  (spill store)
    //   MOVDQU_LOAD_MR  = 0x89
    //   MOVDQU_STORE_RM = 0x8A
    //   MOVAPS_LOAD_MR  = 0x8B
    //   LEA_FRAME_MR    = 0xA0  (materialises address, not a real load but touches frame)
    //   MOV_LOAD_REG_MR = 0xA1
    //   MOV_STORE_REG_RM = 0xA2
    //   MOVSD_LOAD_MR   = 0xB6
    //   MOVSD_STORE_RM  = 0xB7
    //   MOVSS_LOAD_MR   = 0xC6
    //   MOVSS_STORE_RM  = 0xC7
    matches!(
        opcode.0,
        0x72 | 0x73 | 0x89 | 0x8A | 0x8B | 0xA0 | 0xA1 | 0xA2 | 0xB6 | 0xB7 | 0xC6 | 0xC7
    )
}

/// Returns `true` if the opcode is a control-flow instruction (branch, call, ret).
///
/// x86-64:  JMP=0x50, JCC=0x51, CALL_DIRECT=0x52, CALL_R=0x53, RET=0x54
/// These are treated as terminators that must depend on all prior instructions
/// in the block.
fn is_terminator(opcode: MOpcode) -> bool {
    // x86: 0x50-0x54
    // AArch64 branch opcodes (see llvm-target-arm/src/instructions.rs) —
    // we use a generous range; the worst case is a spurious Ctrl edge.
    // For robustness across targets we check any Block operand as a proxy for
    // "this is a branch"; we also hard-code the known x86 control-flow range.
    matches!(opcode.0, 0x50..=0x54)
}

/// Returns `true` if an instruction has a `Block` operand (targets a basic block),
/// which is the target-independent signal for a branch instruction.
fn has_block_operand(instr: &MInstr) -> bool {
    instr.operands.iter().any(|op| matches!(op, MOperand::Block(_)))
}

// ── dependency DAG construction ────────────────────────────────────────────

/// Build a dependency graph for a sequence of machine instructions.
///
/// Returns `successors[i]` — the list of [`DepEdge`]s that originate at
/// instruction `i` (i.e. edges `i → j` where `j` depends on `i`).
///
/// The `latency_fn` callback is called with the *producing* instruction's
/// opcode to obtain the RAW latency for that edge.  WAR and WAW edges and
/// conservative memory/control edges all use a latency of 1.
pub fn build_dep_dag(
    instrs: &[MInstr],
    latency_fn: &dyn Fn(MOpcode) -> u32,
) -> Vec<Vec<DepEdge>> {
    let n = instrs.len();
    let mut succ: Vec<Vec<DepEdge>> = vec![Vec::new(); n];

    // Pre-compute read/write sets and memory/terminator flags.
    let wr: Vec<Vec<u64>> = instrs.iter().map(writes).collect();
    let rd: Vec<Vec<u64>> = instrs.iter().map(reads).collect();
    let is_mem: Vec<bool> = instrs.iter().map(|mi| is_memory_op(mi.opcode)).collect();
    let is_ctrl: Vec<bool> = instrs
        .iter()
        .map(|mi| is_terminator(mi.opcode) || has_block_operand(mi))
        .collect();

    for i in 0..n {
        for j in (i + 1)..n {
            // RAW: i writes something that j reads.
            let raw = wr[i].iter().any(|w| rd[j].contains(w));
            if raw {
                succ[i].push(DepEdge {
                    to: j,
                    kind: DepKind::Raw,
                    latency: latency_fn(instrs[i].opcode),
                });
                // Don't also add WAW or WAR if RAW already present — avoid
                // duplicate edges to the same `j` for the same resource.
                continue;
            }

            // WAW: both write the same register.
            let waw = wr[i].iter().any(|w| wr[j].contains(w));
            if waw {
                succ[i].push(DepEdge { to: j, kind: DepKind::Waw, latency: 1 });
                continue;
            }

            // WAR: i reads something that j writes.
            let war = rd[i].iter().any(|r| wr[j].contains(r));
            if war {
                succ[i].push(DepEdge { to: j, kind: DepKind::War, latency: 1 });
                // Still fall through to check Mem/Ctrl — they're orthogonal.
                // But we skip adding another reg dep for the same pair.
                continue;
            }

            // Mem: conservative — any two memory ops in order have a dependency.
            if is_mem[i] && is_mem[j] {
                succ[i].push(DepEdge { to: j, kind: DepKind::Mem, latency: 1 });
                continue;
            }

            // Ctrl: every prior instruction must precede a terminator.
            if is_ctrl[j] {
                succ[i].push(DepEdge { to: j, kind: DepKind::Ctrl, latency: 1 });
            }
        }
    }

    succ
}

// ── critical-path computation ──────────────────────────────────────────────

/// Compute the critical-path length from each node to a sink.
///
/// `critical_path[i]` is the maximum sum of edge latencies on any path from
/// node `i` to a leaf (node with no successors).  Leaves have value 0.
///
/// The DAG is a DAG by construction (i < j for every edge i→j), so we can
/// compute critical paths in reverse topological order simply by iterating
/// from `n-1` down to `0`.
pub fn compute_critical_paths(dag: &[Vec<DepEdge>]) -> Vec<u32> {
    let n = dag.len();
    let mut cp = vec![0u32; n];
    // Iterate in reverse: sinks first.
    for i in (0..n).rev() {
        for edge in &dag[i] {
            let candidate = edge.latency + cp[edge.to];
            if candidate > cp[i] {
                cp[i] = candidate;
            }
        }
    }
    cp
}

// ── list scheduler ─────────────────────────────────────────────────────────

/// Schedule instructions using a critical-path list scheduler.
///
/// Returns a permuted `Vec<usize>` of instruction indices (0-based) giving the
/// new emission order.  Dependencies are always respected.
///
/// **Algorithm**:
/// 1. Compute in-degree for every node.
/// 2. Seed the ready set with all zero-in-degree nodes.
/// 3. Repeatedly dequeue the highest-priority ready node (priority =
///    `critical_path[i]`; ties broken by original index for determinism),
///    emit it, and decrement the in-degree of its successors — adding any
///    newly-ready ones to the ready set.
pub fn list_schedule(
    instrs: &[MInstr],
    dag: &[Vec<DepEdge>],
    _latency_fn: &dyn Fn(MOpcode) -> u32,
) -> Vec<usize> {
    let n = instrs.len();
    if n == 0 {
        return Vec::new();
    }

    let cp = compute_critical_paths(dag);

    // Compute in-degree of each node.
    let mut in_deg = vec![0usize; n];
    for succs in dag {
        for e in succs {
            in_deg[e.to] += 1;
        }
    }

    // BinaryHeap entries: (critical_path, negated_index) so that higher
    // critical-path wins and lower index breaks ties (stable ordering).
    let mut heap: BinaryHeap<(u32, usize)> = BinaryHeap::new();
    for i in 0..n {
        if in_deg[i] == 0 {
            // Use `n - i` so that lower `i` values yield a larger second key,
            // breaking ties in favour of earlier original position.
            heap.push((cp[i], n - i));
        }
    }

    let mut order = Vec::with_capacity(n);
    // Map heap key back to actual instruction index.
    // We store (cp, tiebreak) → the actual index is n - tiebreak.
    // Re-derive index from the tiebreak field: idx = n - tiebreak.
    while let Some((_, tiebreak)) = heap.pop() {
        let i = n - tiebreak;
        order.push(i);
        for edge in &dag[i] {
            let j = edge.to;
            in_deg[j] -= 1;
            if in_deg[j] == 0 {
                heap.push((cp[j], n - j));
            }
        }
    }

    // Safety: if the DAG is acyclic (it always is by construction) every node
    // will have been scheduled.
    debug_assert_eq!(order.len(), n, "list_schedule: not all instructions were scheduled");
    order
}

// ── apply schedule ─────────────────────────────────────────────────────────

/// Reorder `block.instrs` according to `order`.
///
/// `order` must be a permutation of `0..block.instrs.len()`.
pub fn apply_schedule(block: &mut MachineBlock, order: &[usize]) {
    debug_assert_eq!(order.len(), block.instrs.len(), "order length must match instrs length");
    let old = std::mem::take(&mut block.instrs);
    block.instrs = order.iter().map(|&i| old[i].clone()).collect();
}

// ── default x86-64 latency table ─────────────────────────────────────────

/// Default latency function for x86-64 (Skylake-ish approximations).
///
/// Opcode constants are those defined in
/// `llvm-target-x86/src/instructions.rs`.
pub fn x86_latency(opcode: MOpcode) -> u32 {
    // Latency table: opcode range → cycles.
    // Reference: Intel Skylake/Cascade Lake instruction tables.
    match opcode.0 {
        // ── data movement (1 cycle) ──
        0x00 | // MOV_RR
        0x01 | // MOV_RI
        0x02 | // MOVSX_32
        0x03 | // MOVSX_8
        0x04 | // MOVZX_8
        0x05 | // MOV_PR
        0x06   // MOVSX_16
        => 1,

        // ── integer arithmetic ──
        0x10 | // ADD_RR
        0x11 | // ADD_RI
        0x12 | // SUB_RR
        0x13 | // SUB_RI
        0x17 | // NEG_R
        0x18   // CQO
        => 1,
        0x14 | // IMUL_RR
        0x15   // IMUL_RRI
        => 3,
        0x16 | // IDIV_R
        0x19   // DIV_R
        => 20,

        // ── bitwise (1 cycle) ──
        0x20..=0x26 => 1,

        // ── shifts (1 cycle) ──
        0x30..=0x35 => 1,

        // ── comparisons (1 cycle) ──
        0x40..=0x43 => 1,

        // ── control flow (1 cycle) ──
        0x50..=0x54 => 1,

        // ── stack (1 cycle) ──
        0x60 | 0x61 => 1,

        // ── misc ──
        0x70 | // NOP
        0x71 | // LEA_RI
        0x74   // INLINE_ASM
        => 1,

        // ── spill loads/stores (4 cycles: L1 hit) ──
        0x72 | // MOV_LOAD_MR
        0x73   // MOV_STORE_RM
        => 4,

        // ── SIMD integer ──
        0x80 | // PADDD_RR
        0x81   // PSUBD_RR
        => 1,
        0x82 => 5,   // PMULLD_RR
        0x83 | // ADDPS_RR
        0x84 | // MULPS_RR
        0x86 | // ADDPD_RR
        0x87   // MULPD_RR
        => 4,
        0x85 => 11,  // DIVPS_RR
        0x88 | // MOVAPS_RR
        0x89 | // MOVDQU_LOAD_MR
        0x8A | // MOVDQU_STORE_RM
        0x8B   // MOVAPS_LOAD_MR
        => 4,

        // ── atomics (conservative: fence-like latency) ──
        0x90..=0x96 | 0x9A => 20,

        // ── non-promotable frame access ──
        0xA0 | // LEA_FRAME_MR
        0xA1 | // MOV_LOAD_REG_MR
        0xA2   // MOV_STORE_REG_RM
        => 4,

        // ── SSE2 double ──
        0xB0 | // ADDSD_RR
        0xB1   // SUBSD_RR
        => 4,
        0xB2 => 4,   // MULSD_RR
        0xB3 => 13,  // DIVSD_RR
        0xB4 => 13,  // SQRTSD_R
        0xB5 => 1,   // UCOMISD_RR
        0xB6 | // MOVSD_LOAD_MR
        0xB7   // MOVSD_STORE_RM
        => 4,

        // ── SSE2 single ──
        0xC0 | 0xC1 => 4, // ADDSS/SUBSS
        0xC2 => 4,        // MULSS
        0xC3 => 11,       // DIVSS
        0xC4 => 11,       // SQRTSS
        0xC5 => 1,        // UCOMISS
        0xC6 | 0xC7 => 4, // MOVSS load/store

        // ── FP ↔ integer conversions ──
        0xD0..=0xD7 => 4,

        // Default: assume 1 cycle.
        _ => 1,
    }
}

// ── tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::isel::{MInstr, MOpcode, MachineBlock, PReg, VReg};

    // A simple constant latency function for testing (every op = 1 cycle).
    fn unit_latency(_: MOpcode) -> u32 { 1 }

    // ── test 1: RAW dependency ─────────────────────────────────────────────

    #[test]
    fn raw_dep_a_writes_b_reads() {
        // A writes VReg(10), B reads VReg(10).
        let v10 = VReg(10);
        let a = MInstr::new(MOpcode(0x10)).with_dst(v10);
        let b = MInstr::new(MOpcode(0x10)).with_vreg(v10);

        let instrs = vec![a, b];
        let dag = build_dep_dag(&instrs, &unit_latency);

        // There must be an edge from 0 → 1 with DepKind::Raw.
        assert_eq!(dag[0].len(), 1, "instruction 0 must have one successor");
        let e = &dag[0][0];
        assert_eq!(e.to, 1);
        assert_eq!(e.kind, DepKind::Raw);
        // No successors for node 1.
        assert!(dag[1].is_empty());
    }

    // ── test 2: WAR dependency ─────────────────────────────────────────────

    #[test]
    fn war_dep_a_reads_b_writes() {
        // A reads PReg(1), B writes PReg(1).
        // We model this by making B have dst=VReg(1) (which encodes PReg(1))
        // and A have operand PReg(1).
        let a = MInstr::new(MOpcode(0x00)).with_preg(PReg(1));
        let b = MInstr::new(MOpcode(0x00)).with_dst(VReg(1)); // writes preg-equivalent

        // Since reads(a) = {preg_key(PReg(1))} and writes(b) = {vreg_key(VReg(1))},
        // and preg_key ≠ vreg_key, use a consistent encoding.
        // Instead: encode both as VReg reads/writes to share the key space.
        let v1 = VReg(100);
        let a2 = MInstr::new(MOpcode(0x00)).with_vreg(v1); // reads v1
        let b2 = MInstr::new(MOpcode(0x00)).with_dst(v1);  // writes v1 → no operand read → WAR

        // Actually WAR: a reads v1, b writes v1; since a has no dst we need
        // to check: writes(a2) is empty (no dst), reads(a2) = {v1}.
        // writes(b2) = {v1}, reads(b2) = {} → WAR edge.
        let instrs = vec![a2, b2];
        let dag = build_dep_dag(&instrs, &unit_latency);

        assert_eq!(dag[0].len(), 1, "WAR edge must exist from 0 → 1");
        let e = &dag[0][0];
        assert_eq!(e.to, 1);
        assert_eq!(e.kind, DepKind::War);
        assert!(dag[1].is_empty());

        // Suppress unused-variable warnings for the PReg versions above.
        let _ = (a, b);
    }

    // ── test 3: WAW dependency ─────────────────────────────────────────────

    #[test]
    fn waw_dep_both_write() {
        // Both instructions write VReg(5).
        let v5 = VReg(5);
        let a = MInstr::new(MOpcode(0x10)).with_dst(v5);
        let b = MInstr::new(MOpcode(0x10)).with_dst(v5);

        let instrs = vec![a, b];
        let dag = build_dep_dag(&instrs, &unit_latency);

        assert_eq!(dag[0].len(), 1);
        let e = &dag[0][0];
        assert_eq!(e.to, 1);
        assert_eq!(e.kind, DepKind::Waw);
        assert!(dag[1].is_empty());
    }

    // ── test 4: Mem dependency ─────────────────────────────────────────────

    #[test]
    fn mem_dep_store_then_load() {
        // A is a store (opcode 0x73 = MOV_STORE_RM), B is a load (0x72 = MOV_LOAD_MR).
        // They have no register dependency — only a memory dependency.
        let a = MInstr::new(MOpcode(0x73)); // store, no dst, no VReg operands
        let b = MInstr::new(MOpcode(0x72)); // load

        let instrs = vec![a, b];
        let dag = build_dep_dag(&instrs, &unit_latency);

        assert_eq!(dag[0].len(), 1, "memory dependency must produce an edge");
        let e = &dag[0][0];
        assert_eq!(e.to, 1);
        assert_eq!(e.kind, DepKind::Mem);
    }

    // ── test 5: Ctrl dependency ────────────────────────────────────────────

    #[test]
    fn ctrl_dep_terminator_depends_on_all() {
        // Three instructions: a, b, term (a branch that uses Block operand).
        let a = MInstr::new(MOpcode(0x10)); // ADD_RR (no deps)
        let b = MInstr::new(MOpcode(0x12)); // SUB_RR (no deps)
        // Terminator: JMP (0x50) with a Block operand.
        let term = MInstr::new(MOpcode(0x50)).with_block(0);

        let instrs = vec![a, b, term];
        let dag = build_dep_dag(&instrs, &unit_latency);

        // a (0) must have a Ctrl edge to term (2).
        let a_to_term = dag[0].iter().any(|e| e.to == 2 && e.kind == DepKind::Ctrl);
        assert!(a_to_term, "instruction a must have Ctrl edge to terminator");

        // b (1) must have a Ctrl edge to term (2).
        let b_to_term = dag[1].iter().any(|e| e.to == 2 && e.kind == DepKind::Ctrl);
        assert!(b_to_term, "instruction b must have Ctrl edge to terminator");
    }

    // ── test 6: independent instructions can be reordered ─────────────────

    #[test]
    fn list_schedule_independent_instrs_can_reorder() {
        // A reads VReg(0), B reads VReg(1) — no shared writes.
        // The scheduler is free to emit them in either order.
        let v0 = VReg(0);
        let v1 = VReg(1);
        let a = MInstr::new(MOpcode(0x10)).with_vreg(v0); // reads v0
        let b = MInstr::new(MOpcode(0x10)).with_vreg(v1); // reads v1

        let instrs = vec![a, b];
        let dag = build_dep_dag(&instrs, &unit_latency);

        // No edges — they are fully independent.
        assert!(dag[0].is_empty(), "no dep from A to B expected");
        assert!(dag[1].is_empty(), "no dep from B to A expected");

        let order = list_schedule(&instrs, &dag, &unit_latency);
        assert_eq!(order.len(), 2);
        // Both indices must appear.
        assert!(order.contains(&0));
        assert!(order.contains(&1));
    }

    // ── test 7: dependency chain preserves order ──────────────────────────

    #[test]
    fn list_schedule_dep_chain_preserves_order() {
        // A writes v0, B reads v0 and writes v1, C reads v1.
        // Must be scheduled A → B → C.
        let v0 = VReg(0);
        let v1 = VReg(1);
        let a = MInstr::new(MOpcode(0x10)).with_dst(v0);
        let b = MInstr::new(MOpcode(0x10)).with_dst(v1).with_vreg(v0);
        let c = MInstr::new(MOpcode(0x10)).with_vreg(v1);

        let instrs = vec![a, b, c];
        let dag = build_dep_dag(&instrs, &unit_latency);
        let order = list_schedule(&instrs, &dag, &unit_latency);

        assert_eq!(order.len(), 3);
        // Find positions in the schedule.
        let pos: Vec<usize> = (0..3).map(|i| order.iter().position(|&x| x == i).unwrap()).collect();
        assert!(pos[0] < pos[1], "A must come before B");
        assert!(pos[1] < pos[2], "B must come before C");
    }

    // ── test 8: critical path prefers long chain ───────────────────────────

    #[test]
    fn critical_path_prefers_long_chain() {
        // Build a scenario where there are two ready instructions:
        //   chain: A → B → C  (critical path = 2 for A)
        //   leaf:  D            (critical path = 0, no successors)
        // Both A and D are ready at start. The scheduler should pick A first.
        let v0 = VReg(0);
        let v1 = VReg(1);
        let a = MInstr::new(MOpcode(0x10)).with_dst(v0);
        let b = MInstr::new(MOpcode(0x10)).with_dst(v1).with_vreg(v0);
        let c = MInstr::new(MOpcode(0x10)).with_vreg(v1);
        let d = MInstr::new(MOpcode(0x12)); // independent, no deps

        // Layout: [d, a, b, c] — d comes first in original order but has lower CP.
        let instrs = vec![d, a, b, c];
        let dag = build_dep_dag(&instrs, &unit_latency);
        let cp = compute_critical_paths(&dag);

        // a (index 1) has successors b→c, so CP should be ≥ 2.
        // d (index 0) has no successors, so CP = 0.
        assert!(cp[1] >= 2, "chain head should have critical path >= 2, got {}", cp[1]);
        assert_eq!(cp[0], 0, "independent instruction d should have CP = 0");

        let order = list_schedule(&instrs, &dag, &unit_latency);
        let pos_a = order.iter().position(|&x| x == 1).unwrap();
        let pos_d = order.iter().position(|&x| x == 0).unwrap();
        assert!(pos_a < pos_d, "chain head 'a' (CP={}) should be scheduled before 'd' (CP=0)", cp[1]);
    }

    // ── test 9: apply_schedule reorders block ──────────────────────────────

    #[test]
    fn apply_schedule_reorders_block() {
        let mut block = MachineBlock {
            label: "test".into(),
            instrs: vec![
                MInstr::new(MOpcode(0x10)), // instr 0: ADD
                MInstr::new(MOpcode(0x12)), // instr 1: SUB
                MInstr::new(MOpcode(0x14)), // instr 2: IMUL
            ],
        };

        // Apply schedule [2, 0, 1]: IMUL, ADD, SUB.
        apply_schedule(&mut block, &[2, 0, 1]);

        assert_eq!(block.instrs.len(), 3);
        assert_eq!(block.instrs[0].opcode, MOpcode(0x14)); // was 2
        assert_eq!(block.instrs[1].opcode, MOpcode(0x10)); // was 0
        assert_eq!(block.instrs[2].opcode, MOpcode(0x12)); // was 1
    }

    // ── additional: x86_latency table sanity checks ────────────────────────

    #[test]
    fn x86_latency_imul_is_3() {
        assert_eq!(x86_latency(MOpcode(0x14)), 3); // IMUL_RR
    }

    #[test]
    fn x86_latency_idiv_is_20() {
        assert_eq!(x86_latency(MOpcode(0x16)), 20); // IDIV_R
    }

    #[test]
    fn x86_latency_load_is_4() {
        assert_eq!(x86_latency(MOpcode(0x72)), 4); // MOV_LOAD_MR
    }

    #[test]
    fn x86_latency_add_is_1() {
        assert_eq!(x86_latency(MOpcode(0x10)), 1); // ADD_RR
    }
}
