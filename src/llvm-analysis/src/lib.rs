//! Analysis passes: CFG, dominator tree, use-def chains, and loop detection.

pub mod cfg;
pub mod dominators;
pub mod loops;
pub mod use_def;

pub use cfg::Cfg;
pub use dominators::DomTree;
pub use loops::{Loop, LoopInfo};
pub use use_def::UseDefInfo;
