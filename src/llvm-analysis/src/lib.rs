//! Analysis passes: CFG, dominator tree, use-def chains, loop detection, and alias analysis.

/// Public API for `alias`.
pub mod alias;
pub mod call_graph;
/// Public API for `cfg`.
pub mod cfg;
/// Public API for `dominators`.
pub mod dominators;
/// Public API for `loops`.
pub mod loops;
/// Public API for `use_def`.
pub mod use_def;

/// Public API for `re-export`.
pub use alias::{
    AliasAnalysis, AliasResult, BasicAliasAnalysis, CombinedAliasAnalysis, MemAccess,
    TbaaAccessTag, TbaaHierarchy, TbaaNode, UnderlyingObject, underlying_object,
};
/// Public API for `re-export`.
pub use call_graph::{CallEdge, CallEdgeKind, CallGraph};
/// Public API for `re-export`.
pub use cfg::Cfg;
/// Public API for `re-export`.
pub use dominators::DomTree;
/// Public API for `re-export`.
pub use loops::{Loop, LoopInfo};
/// Public API for `re-export`.
pub use use_def::UseDefInfo;
