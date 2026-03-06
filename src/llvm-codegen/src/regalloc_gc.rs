//! Graph-coloring register allocator (Chaitin-Briggs style).

use crate::isel::{PReg, VReg};
use crate::regalloc::{LiveInterval, RegAllocResult};
use std::collections::{HashMap, HashSet};

pub type InterferenceGraph = HashMap<VReg, HashSet<VReg>>;

fn overlaps(a: &LiveInterval, b: &LiveInterval) -> bool {
    a.start < b.end && b.start < a.end
}

pub fn build_interference_graph(intervals: &[LiveInterval]) -> InterferenceGraph {
    let mut graph: InterferenceGraph = HashMap::new();
    for iv in intervals {
        graph.entry(iv.vreg).or_default();
    }

    for i in 0..intervals.len() {
        for j in (i + 1)..intervals.len() {
            let a = &intervals[i];
            let b = &intervals[j];
            if overlaps(a, b) {
                graph.entry(a.vreg).or_default().insert(b.vreg);
                graph.entry(b.vreg).or_default().insert(a.vreg);
            }
        }
    }

    graph
}

/// Compute spill costs from interval length and use density.
/// Longer intervals are considered more expensive to spill.
pub fn spill_costs(intervals: &[LiveInterval]) -> HashMap<VReg, usize> {
    let mut costs = HashMap::new();
    for iv in intervals {
        let len = iv.end.saturating_sub(iv.start).max(1);
        costs.insert(iv.vreg, len);
    }
    costs
}

/// Chaitin-Briggs style graph-coloring allocator.
///
/// The returned type matches linear-scan, so spill/reload insertion and
/// allocation application can remain unchanged.
pub fn graph_color(intervals: &[LiveInterval], allocatable: &[PReg]) -> RegAllocResult {
    if allocatable.is_empty() {
        return RegAllocResult {
            vreg_to_preg: HashMap::new(),
            spilled: intervals.iter().map(|iv| iv.vreg).collect(),
        };
    }
    if intervals.is_empty() {
        return RegAllocResult::default();
    }

    let k = allocatable.len();
    let graph = build_interference_graph(intervals);
    let costs = spill_costs(intervals);

    let mut work = graph.clone();
    let mut stack: Vec<VReg> = Vec::with_capacity(work.len());
    let mut potential_spills: HashSet<VReg> = HashSet::new();

    while !work.is_empty() {
        let low_degree = work
            .iter()
            .filter(|(_, neigh)| neigh.len() < k)
            .map(|(vr, neigh)| (*vr, neigh.len()))
            .min_by_key(|(vr, degree)| (*degree, vr.0));

        let chosen = if let Some((vr, _)) = low_degree {
            vr
        } else {
            let spill_vr = work
                .iter()
                .map(|(vr, neigh)| {
                    let cost = *costs.get(vr).unwrap_or(&1);
                    (*vr, cost, neigh.len())
                })
                .min_by_key(|(vr, cost, degree)| (*cost, std::cmp::Reverse(*degree), vr.0))
                .map(|(vr, _, _)| vr)
                .expect("non-empty graph must pick a node");
            potential_spills.insert(spill_vr);
            spill_vr
        };

        if let Some(neighs) = work.remove(&chosen) {
            for n in neighs {
                if let Some(nset) = work.get_mut(&n) {
                    nset.remove(&chosen);
                }
            }
        }
        stack.push(chosen);
    }

    let mut assigned: HashMap<VReg, PReg> = HashMap::new();
    let mut spilled: HashSet<VReg> = HashSet::new();

    while let Some(vr) = stack.pop() {
        let forbidden: HashSet<PReg> = graph
            .get(&vr)
            .into_iter()
            .flat_map(|neigh| neigh.iter())
            .filter_map(|n| assigned.get(n).copied())
            .collect();

        if let Some(&pr) = allocatable.iter().find(|pr| !forbidden.contains(pr)) {
            assigned.insert(vr, pr);
        } else {
            spilled.insert(vr);
        }
    }

    // Keep potential spills that could not be assigned as actual spills.
    for vr in potential_spills {
        if !assigned.contains_key(&vr) {
            spilled.insert(vr);
        }
    }

    RegAllocResult {
        vreg_to_preg: assigned,
        spilled: {
            let mut out: Vec<VReg> = spilled.into_iter().collect();
            out.sort_unstable_by_key(|vr| vr.0);
            out
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::regalloc::{allocate_registers, linear_scan, RegAllocStrategy};

    fn iv(vreg: u32, start: usize, end: usize) -> LiveInterval {
        LiveInterval {
            vreg: VReg(vreg),
            start,
            end,
        }
    }

    #[test]
    fn graph_builds_empty() {
        let g = build_interference_graph(&[]);
        assert!(g.is_empty());
    }

    #[test]
    fn graph_adds_overlap_edge() {
        let g = build_interference_graph(&[iv(0, 0, 4), iv(1, 2, 5)]);
        assert!(g[&VReg(0)].contains(&VReg(1)));
        assert!(g[&VReg(1)].contains(&VReg(0)));
    }

    #[test]
    fn graph_omits_non_overlap_edge() {
        let g = build_interference_graph(&[iv(0, 0, 2), iv(1, 2, 4)]);
        assert!(g[&VReg(0)].is_empty());
        assert!(g[&VReg(1)].is_empty());
    }

    #[test]
    fn graph_touching_endpoints_do_not_interfere() {
        let a = iv(0, 3, 7);
        let b = iv(1, 7, 9);
        assert!(!overlaps(&a, &b));
    }

    #[test]
    fn spill_cost_favors_longer_ranges() {
        let costs = spill_costs(&[iv(0, 0, 2), iv(1, 0, 10)]);
        assert!(costs[&VReg(1)] > costs[&VReg(0)]);
    }

    #[test]
    fn graph_color_empty_intervals() {
        let r = graph_color(&[], &[PReg(0)]);
        assert!(r.vreg_to_preg.is_empty());
        assert!(r.spilled.is_empty());
    }

    #[test]
    fn graph_color_no_registers_spills_all() {
        let intervals = vec![iv(0, 0, 4), iv(1, 1, 3)];
        let r = graph_color(&intervals, &[]);
        assert_eq!(r.vreg_to_preg.len(), 0);
        assert_eq!(r.spilled.len(), 2);
    }

    #[test]
    fn graph_color_non_overlapping_share_one_reg() {
        let intervals = vec![iv(0, 0, 2), iv(1, 2, 4), iv(2, 4, 6)];
        let r = graph_color(&intervals, &[PReg(0)]);
        assert!(r.spilled.is_empty());
        assert_eq!(r.vreg_to_preg.len(), 3);
    }

    #[test]
    fn graph_color_triangle_two_regs_spills_one() {
        let intervals = vec![iv(0, 0, 6), iv(1, 1, 7), iv(2, 2, 8)];
        let r = graph_color(&intervals, &[PReg(0), PReg(1)]);
        assert_eq!(r.spilled.len(), 1);
    }

    #[test]
    fn graph_color_triangle_three_regs_spills_none() {
        let intervals = vec![iv(0, 0, 6), iv(1, 1, 7), iv(2, 2, 8)];
        let r = graph_color(&intervals, &[PReg(0), PReg(1), PReg(2)]);
        assert!(r.spilled.is_empty());
    }

    #[test]
    fn graph_color_deterministic_for_same_input() {
        let intervals = vec![iv(3, 0, 5), iv(0, 1, 4), iv(2, 2, 6), iv(1, 5, 7)];
        let regs = vec![PReg(0), PReg(1)];
        let a = graph_color(&intervals, &regs);
        let b = graph_color(&intervals, &regs);
        assert_eq!(a.spilled, b.spilled);
        assert_eq!(a.vreg_to_preg, b.vreg_to_preg);
    }

    #[test]
    fn graph_color_respects_interference_assignments() {
        let intervals = vec![iv(0, 0, 5), iv(1, 0, 5), iv(2, 5, 8)];
        let r = graph_color(&intervals, &[PReg(0), PReg(1)]);
        let p0 = r.vreg_to_preg[&VReg(0)];
        let p1 = r.vreg_to_preg[&VReg(1)];
        assert_ne!(p0, p1);
    }

    #[test]
    fn graph_color_can_recover_potential_spill() {
        // 0 and 2 do not interfere, so one color can be reused.
        let intervals = vec![iv(0, 0, 3), iv(1, 1, 4), iv(2, 4, 6)];
        let r = graph_color(&intervals, &[PReg(0), PReg(1)]);
        assert!(r.spilled.is_empty());
        assert_eq!(r.vreg_to_preg.len(), 3);
    }

    #[test]
    fn strategy_dispatch_uses_graph_color() {
        let intervals = vec![iv(0, 0, 6), iv(1, 1, 7), iv(2, 2, 8)];
        let regs = vec![PReg(0), PReg(1)];
        let r = allocate_registers(&intervals, &regs, RegAllocStrategy::GraphColor);
        assert_eq!(r.spilled.len(), 1);
    }

    #[test]
    fn strategy_dispatch_linear_scan_default() {
        let intervals = vec![iv(0, 0, 4), iv(1, 2, 6)];
        let regs = vec![PReg(0)];
        let a = allocate_registers(&intervals, &regs, RegAllocStrategy::default());
        let b = linear_scan(&intervals, &regs);
        assert_eq!(a.spilled, b.spilled);
        assert_eq!(a.vreg_to_preg, b.vreg_to_preg);
    }

    #[test]
    fn graph_color_spills_no_more_than_linear_scan_case1() {
        let intervals = vec![
            iv(0, 0, 8),
            iv(1, 1, 5),
            iv(2, 2, 6),
            iv(3, 6, 10),
            iv(4, 8, 11),
        ];
        let regs = vec![PReg(0), PReg(1)];
        let gc = graph_color(&intervals, &regs);
        let ls = linear_scan(&intervals, &regs);
        assert!(gc.spilled.len() <= ls.spilled.len());
    }

    #[test]
    fn graph_color_spills_no_more_than_linear_scan_case2() {
        let intervals = vec![
            iv(0, 0, 7),
            iv(1, 0, 7),
            iv(2, 1, 3),
            iv(3, 3, 5),
            iv(4, 5, 9),
            iv(5, 6, 10),
        ];
        let regs = vec![PReg(0), PReg(1), PReg(2)];
        let gc = graph_color(&intervals, &regs);
        let ls = linear_scan(&intervals, &regs);
        assert!(gc.spilled.len() <= ls.spilled.len());
    }
}
