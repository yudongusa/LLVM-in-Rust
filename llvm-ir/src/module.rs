//! Module: top-level container for globals, functions, and metadata.

use std::collections::HashMap;
use crate::context::{FunctionId, GlobalId, TypeId};
use crate::function::Function;
use crate::value::GlobalVariable;

/// Top-level IR module.
pub struct Module {
    pub name: String,
    pub source_filename: Option<String>,
    pub target_triple: Option<String>,
    pub data_layout: Option<String>,
    pub globals: Vec<GlobalVariable>,
    pub functions: Vec<Function>,
    pub function_names: HashMap<String, FunctionId>,
    pub global_names: HashMap<String, GlobalId>,
    /// Named type definitions in declaration order (for printing).
    pub named_types: Vec<(String, TypeId)>,
}

impl Module {
    pub fn new(name: impl Into<String>) -> Self {
        Module {
            name: name.into(),
            source_filename: None,
            target_triple: None,
            data_layout: None,
            globals: Vec::new(),
            functions: Vec::new(),
            function_names: HashMap::new(),
            global_names: HashMap::new(),
            named_types: Vec::new(),
        }
    }

    // -----------------------------------------------------------------------
    // Functions
    // -----------------------------------------------------------------------

    pub fn add_function(&mut self, f: Function) -> FunctionId {
        let id = FunctionId(self.functions.len() as u32);
        self.function_names.insert(f.name.clone(), id);
        self.functions.push(f);
        id
    }

    pub fn function(&self, id: FunctionId) -> &Function {
        &self.functions[id.0 as usize]
    }

    pub fn function_mut(&mut self, id: FunctionId) -> &mut Function {
        &mut self.functions[id.0 as usize]
    }

    pub fn get_function(&self, name: &str) -> Option<(FunctionId, &Function)> {
        self.function_names
            .get(name)
            .map(|&id| (id, &self.functions[id.0 as usize]))
    }

    pub fn get_function_id(&self, name: &str) -> Option<FunctionId> {
        self.function_names.get(name).copied()
    }

    pub fn num_functions(&self) -> usize {
        self.functions.len()
    }

    // -----------------------------------------------------------------------
    // Globals
    // -----------------------------------------------------------------------

    pub fn add_global(&mut self, gv: GlobalVariable) -> GlobalId {
        let id = GlobalId(self.globals.len() as u32);
        self.global_names.insert(gv.name.clone(), id);
        self.globals.push(gv);
        id
    }

    pub fn global(&self, id: GlobalId) -> &GlobalVariable {
        &self.globals[id.0 as usize]
    }

    pub fn global_mut(&mut self, id: GlobalId) -> &mut GlobalVariable {
        &mut self.globals[id.0 as usize]
    }

    pub fn get_global(&self, name: &str) -> Option<(GlobalId, &GlobalVariable)> {
        self.global_names
            .get(name)
            .map(|&id| (id, &self.globals[id.0 as usize]))
    }

    pub fn get_global_id(&self, name: &str) -> Option<GlobalId> {
        self.global_names.get(name).copied()
    }

    pub fn num_globals(&self) -> usize {
        self.globals.len()
    }

    // -----------------------------------------------------------------------
    // Named types
    // -----------------------------------------------------------------------

    /// Register a named struct type for emission in the module header.
    /// Duplicate names are silently ignored.
    pub fn register_named_type(&mut self, name: impl Into<String>, ty: TypeId) {
        let name = name.into();
        if !self.named_types.iter().any(|(n, _)| n == &name) {
            self.named_types.push((name, ty));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::Context;
    use crate::value::{GlobalVariable, Linkage};

    #[test]
    fn module_functions() {
        let mut ctx = Context::new();
        let fn_ty = ctx.mk_fn_type(ctx.i32_ty, vec![], false);
        let f = Function::new("foo", fn_ty, vec![], Linkage::External);
        let mut m = Module::new("test");
        let id = m.add_function(f);
        assert_eq!(id, FunctionId(0));
        assert_eq!(m.function(id).name, "foo");
        assert_eq!(m.get_function_id("foo"), Some(FunctionId(0)));
    }

    #[test]
    fn module_globals() {
        let mut ctx = Context::new();
        let gv = GlobalVariable {
            name: "x".to_string(),
            ty: ctx.i32_ty,
            initializer: None,
            is_constant: false,
            linkage: Linkage::External,
        };
        let mut m = Module::new("test");
        let id = m.add_global(gv);
        assert_eq!(id, GlobalId(0));
        assert_eq!(m.global(id).name, "x");
    }
}
