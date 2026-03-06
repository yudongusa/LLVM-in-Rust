//! Module-level call graph with edge kinds and SCC traversal.

use llvm_ir::{Context, FunctionId, InstrKind, Module, ValueRef};
use std::collections::{BTreeSet, HashSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CallEdgeKind {
    Direct,
    Indirect,
    External,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CallEdge {
    pub to: Option<FunctionId>,
    pub kind: CallEdgeKind,
}

/// Call graph over all functions in a module.
pub struct CallGraph {
    direct_callees: Vec<Vec<FunctionId>>,
    direct_callers: Vec<Vec<FunctionId>>,
    edges: Vec<Vec<CallEdge>>,
}

impl CallGraph {
    pub fn build(_ctx: &Context, module: &Module) -> Self {
        let n = module.functions.len();
        let mut direct_callees_sets: Vec<BTreeSet<FunctionId>> = vec![BTreeSet::new(); n];
        let mut direct_callers_sets: Vec<BTreeSet<FunctionId>> = vec![BTreeSet::new(); n];
        let mut edges: Vec<Vec<CallEdge>> = vec![Vec::new(); n];

        for (src_idx, func) in module.functions.iter().enumerate() {
            for instr in &func.instructions {
                let InstrKind::Call { callee, .. } = &instr.kind else {
                    continue;
                };
                let edge = match callee {
                    ValueRef::Global(gid) => {
                        let fid = FunctionId(gid.0);
                        if (fid.0 as usize) < n {
                            let kind = if module.functions[fid.0 as usize].is_declaration {
                                CallEdgeKind::External
                            } else {
                                CallEdgeKind::Direct
                            };
                            direct_callees_sets[src_idx].insert(fid);
                            direct_callers_sets[fid.0 as usize].insert(FunctionId(src_idx as u32));
                            CallEdge {
                                to: Some(fid),
                                kind,
                            }
                        } else {
                            CallEdge {
                                to: None,
                                kind: CallEdgeKind::External,
                            }
                        }
                    }
                    _ => CallEdge {
                        to: None,
                        kind: CallEdgeKind::Indirect,
                    },
                };
                edges[src_idx].push(edge);
            }
        }

        let direct_callees = direct_callees_sets
            .into_iter()
            .map(|s| s.into_iter().collect())
            .collect();
        let direct_callers = direct_callers_sets
            .into_iter()
            .map(|s| s.into_iter().collect())
            .collect();
        Self {
            direct_callees,
            direct_callers,
            edges,
        }
    }

    pub fn callees(&self, f: FunctionId) -> &[FunctionId] {
        &self.direct_callees[f.0 as usize]
    }

    pub fn callers(&self, f: FunctionId) -> &[FunctionId] {
        &self.direct_callers[f.0 as usize]
    }

    pub fn edges(&self, f: FunctionId) -> &[CallEdge] {
        &self.edges[f.0 as usize]
    }

    /// Return SCCs in bottom-up order over the direct-call graph.
    pub fn sccs(&self) -> Vec<Vec<FunctionId>> {
        let n = self.direct_callees.len();
        let mut order = Vec::with_capacity(n);
        let mut seen = vec![false; n];
        for i in 0..n {
            if !seen[i] {
                self.dfs_post(i, &mut seen, &mut order);
            }
        }

        let mut rev_adj: Vec<Vec<usize>> = vec![Vec::new(); n];
        for src in 0..n {
            for &dst in &self.direct_callees[src] {
                rev_adj[dst.0 as usize].push(src);
            }
        }

        let mut comp = vec![usize::MAX; n];
        let mut comp_nodes: Vec<Vec<usize>> = Vec::new();
        for &u in order.iter().rev() {
            if comp[u] != usize::MAX {
                continue;
            }
            let id = comp_nodes.len();
            let mut nodes = Vec::new();
            let mut stack = vec![u];
            comp[u] = id;
            while let Some(x) = stack.pop() {
                nodes.push(x);
                for &p in &rev_adj[x] {
                    if comp[p] == usize::MAX {
                        comp[p] = id;
                        stack.push(p);
                    }
                }
            }
            comp_nodes.push(nodes);
        }

        let c = comp_nodes.len();
        let mut comp_succ: Vec<HashSet<usize>> = vec![HashSet::new(); c];
        let mut indeg = vec![0usize; c];
        for src in 0..n {
            for &dst in &self.direct_callees[src] {
                let a = comp[src];
                let b = comp[dst.0 as usize];
                if a != b && comp_succ[a].insert(b) {
                    indeg[b] += 1;
                }
            }
        }

        // Topological order from roots to leaves; reverse for bottom-up.
        let mut q: Vec<usize> = (0..c).filter(|&i| indeg[i] == 0).collect();
        q.sort_unstable();
        let mut topo = Vec::with_capacity(c);
        while let Some(x) = q.pop() {
            topo.push(x);
            let mut succs: Vec<usize> = comp_succ[x].iter().copied().collect();
            succs.sort_unstable();
            for y in succs {
                indeg[y] -= 1;
                if indeg[y] == 0 {
                    q.push(y);
                }
            }
        }

        topo.reverse();
        topo.into_iter()
            .map(|cid| {
                let mut ids: Vec<FunctionId> = comp_nodes[cid]
                    .iter()
                    .map(|&x| FunctionId(x as u32))
                    .collect();
                ids.sort_unstable_by_key(|f| f.0);
                ids
            })
            .collect()
    }

    fn dfs_post(&self, u: usize, seen: &mut [bool], order: &mut Vec<usize>) {
        if seen[u] {
            return;
        }
        seen[u] = true;
        for &v in &self.direct_callees[u] {
            self.dfs_post(v.0 as usize, seen, order);
        }
        order.push(u);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use llvm_ir::{Builder, GlobalId, Linkage};

    fn build_graph_module() -> (Context, Module) {
        let mut ctx = Context::new();
        let mut module = Module::new("m");
        let mut b = Builder::new(&mut ctx, &mut module);
        let i64_ty = b.ctx.i64_ty;
        let fn_ty = b.ctx.mk_fn_type(i64_ty, vec![i64_ty], false);

        b.add_function("a", i64_ty, vec![i64_ty], vec!["x".into()], false, Linkage::External);
        let a_entry = b.add_block("a.entry");
        b.position_at_end(a_entry);
        let x = b.get_arg(0);
        let c1 = b.build_call("c1", i64_ty, fn_ty, ValueRef::Global(GlobalId(1)), vec![x]);
        let c2 = b.build_call("c2", i64_ty, fn_ty, ValueRef::Global(GlobalId(2)), vec![x]);
        let sum = b.build_add("sum", c1, c2);
        b.build_ret(sum);

        b.add_function("b", i64_ty, vec![i64_ty], vec!["x".into()], false, Linkage::External);
        let b_entry = b.add_block("b.entry");
        b.position_at_end(b_entry);
        let bx = b.get_arg(0);
        let bcall = b.build_call("r", i64_ty, fn_ty, ValueRef::Global(GlobalId(2)), vec![bx]);
        b.build_ret(bcall);

        b.add_function("c", i64_ty, vec![i64_ty], vec!["x".into()], false, Linkage::External);
        let c_entry = b.add_block("c.entry");
        b.position_at_end(c_entry);
        let cx = b.get_arg(0);
        b.build_ret(cx);

        (ctx, module)
    }

    #[test]
    fn call_graph_builds_direct_edges_for_dag() {
        let (ctx, module) = build_graph_module();
        let cg = CallGraph::build(&ctx, &module);

        assert_eq!(cg.callees(FunctionId(0)), &[FunctionId(1), FunctionId(2)]);
        assert_eq!(cg.callees(FunctionId(1)), &[FunctionId(2)]);
        assert!(cg.callees(FunctionId(2)).is_empty());
        assert_eq!(cg.callers(FunctionId(2)), &[FunctionId(0), FunctionId(1)]);
    }

    #[test]
    fn call_graph_sccs_group_cycle_together() {
        let mut ctx = Context::new();
        let mut module = Module::new("m");
        let mut b = Builder::new(&mut ctx, &mut module);
        let i64_ty = b.ctx.i64_ty;
        let fn_ty = b.ctx.mk_fn_type(i64_ty, vec![i64_ty], false);
        for name in ["a", "b", "c"] {
            b.add_function(name, i64_ty, vec![i64_ty], vec!["x".into()], false, Linkage::External);
            let entry = b.add_block(format!("{name}.entry"));
            b.position_at_end(entry);
            let x = b.get_arg(0);
            let callee = match name {
                "a" => ValueRef::Global(GlobalId(1)),
                "b" => ValueRef::Global(GlobalId(0)),
                _ => ValueRef::Global(GlobalId(2)),
            };
            let r = b.build_call("r", i64_ty, fn_ty, callee, vec![x]);
            b.build_ret(r);
        }

        let cg = CallGraph::build(&ctx, &module);
        let sccs = cg.sccs();
        assert!(
            sccs.iter()
                .any(|s| s.len() == 2 && s.contains(&FunctionId(0)) && s.contains(&FunctionId(1)))
        );
    }

    #[test]
    fn call_graph_classifies_indirect_and_external_calls() {
        let mut ctx = Context::new();
        let mut module = Module::new("m");
        let mut b = Builder::new(&mut ctx, &mut module);
        let i64_ty = b.ctx.i64_ty;
        let fn_ty = b.ctx.mk_fn_type(i64_ty, vec![i64_ty], false);
        b.add_declaration("ext", i64_ty, vec![i64_ty], false);

        b.add_function("f", i64_ty, vec![i64_ty], vec!["x".into()], false, Linkage::External);
        let entry = b.add_block("entry");
        b.position_at_end(entry);
        let x = b.get_arg(0);
        let _ext = b.build_call("ext", i64_ty, fn_ty, ValueRef::Global(GlobalId(0)), vec![x]);
        let _ind = b.build_call("ind", i64_ty, fn_ty, x, vec![x]);
        b.build_ret(x);

        let cg = CallGraph::build(&ctx, &module);
        let edges = cg.edges(FunctionId(1));
        assert!(edges.iter().any(|e| e.kind == CallEdgeKind::External));
        assert!(edges.iter().any(|e| e.kind == CallEdgeKind::Indirect));
    }
}
