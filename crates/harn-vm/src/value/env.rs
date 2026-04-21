use std::collections::BTreeMap;
use std::rc::Rc;
use std::{cell::RefCell, path::PathBuf};

use crate::chunk::CompiledFunctionRef;

use super::{VmError, VmValue};

/// A compiled closure value.
#[derive(Debug, Clone)]
pub struct VmClosure {
    pub func: CompiledFunctionRef,
    pub env: VmEnv,
    /// Source directory for this closure's originating module.
    /// When set, `render()` and other source-relative builtins resolve
    /// paths relative to this directory instead of the entry pipeline.
    pub source_dir: Option<PathBuf>,
    /// Module-local named functions that should resolve before builtin fallback.
    /// This lets selectively imported functions keep private sibling helpers
    /// without exporting them into the caller's environment.
    pub module_functions: Option<ModuleFunctionRegistry>,
    /// Shared, mutable module-level env: holds top-level `var` / `let`
    /// bindings declared at the module root (caches, counters, lazily
    /// initialized registries). All closures created from the same
    /// module import point at the same `Rc<RefCell<VmEnv>>`, so a
    /// mutation inside one function is visible to every other function
    /// in that module on subsequent calls. `closure.env` still holds
    /// the per-closure lexical snapshot (captured function args from
    /// enclosing scopes, etc.) and is unchanged by this — `module_state`
    /// is a separate lookup layer consulted after the local env and
    /// before globals. Created in `import_declarations` after the
    /// module's init chunk runs, so the initial values from `var x = ...`
    /// land in it.
    pub module_state: Option<ModuleState>,
}

pub type ModuleFunctionRegistry = Rc<RefCell<BTreeMap<String, Rc<VmClosure>>>>;
pub type ModuleState = Rc<RefCell<VmEnv>>;

/// VM environment for variable storage.
#[derive(Debug, Clone)]
pub struct VmEnv {
    pub(crate) scopes: Vec<Scope>,
}

#[derive(Debug, Clone)]
pub(crate) struct Scope {
    pub(crate) vars: BTreeMap<String, (VmValue, bool)>, // (value, mutable)
}

impl Default for VmEnv {
    fn default() -> Self {
        Self::new()
    }
}

impl VmEnv {
    pub fn new() -> Self {
        Self {
            scopes: vec![Scope {
                vars: BTreeMap::new(),
            }],
        }
    }

    pub fn push_scope(&mut self) {
        self.scopes.push(Scope {
            vars: BTreeMap::new(),
        });
    }

    pub fn pop_scope(&mut self) {
        if self.scopes.len() > 1 {
            self.scopes.pop();
        }
    }

    pub fn scope_depth(&self) -> usize {
        self.scopes.len()
    }

    pub fn truncate_scopes(&mut self, target_depth: usize) {
        let min_depth = target_depth.max(1);
        while self.scopes.len() > min_depth {
            self.scopes.pop();
        }
    }

    pub fn get(&self, name: &str) -> Option<VmValue> {
        for scope in self.scopes.iter().rev() {
            if let Some((val, _)) = scope.vars.get(name) {
                return Some(val.clone());
            }
        }
        None
    }

    pub fn define(&mut self, name: &str, value: VmValue, mutable: bool) -> Result<(), VmError> {
        if let Some(scope) = self.scopes.last_mut() {
            if let Some((_, existing_mutable)) = scope.vars.get(name) {
                if !existing_mutable && !mutable {
                    return Err(VmError::Runtime(format!(
                        "Cannot redeclare immutable variable '{name}' in the same scope (use 'var' for mutable bindings)"
                    )));
                }
            }
            scope.vars.insert(name.to_string(), (value, mutable));
        }
        Ok(())
    }

    pub fn all_variables(&self) -> BTreeMap<String, VmValue> {
        let mut vars = BTreeMap::new();
        for scope in &self.scopes {
            for (name, (value, _)) in &scope.vars {
                vars.insert(name.clone(), value.clone());
            }
        }
        vars
    }

    pub fn assign(&mut self, name: &str, value: VmValue) -> Result<(), VmError> {
        for scope in self.scopes.iter_mut().rev() {
            if let Some((_, mutable)) = scope.vars.get(name) {
                if !mutable {
                    return Err(VmError::ImmutableAssignment(name.to_string()));
                }
                scope.vars.insert(name.to_string(), (value, true));
                return Ok(());
            }
        }
        Err(VmError::UndefinedVariable(name.to_string()))
    }

    /// Debugger-only variant of `assign` that rebinds the name even if
    /// the existing binding was declared with `let`. Pipeline authors
    /// overwhelmingly use `let`, so a strict mutability check would
    /// make the DAP `setVariable` request useless for "what-if"
    /// iteration — which is the whole point of the feature. Preserves
    /// the original mutability flag so the VM's runtime behavior is
    /// unchanged after the debugger overrides.
    pub fn assign_debug(&mut self, name: &str, value: VmValue) -> Result<(), VmError> {
        for scope in self.scopes.iter_mut().rev() {
            if let Some((_, mutable)) = scope.vars.get(name) {
                let mutable = *mutable;
                scope.vars.insert(name.to_string(), (value, mutable));
                return Ok(());
            }
        }
        Err(VmError::UndefinedVariable(name.to_string()))
    }
}

/// Compute Levenshtein edit distance between two strings.
fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let (m, n) = (a.len(), b.len());
    let mut prev = (0..=n).collect::<Vec<_>>();
    let mut curr = vec![0; n + 1];
    for i in 1..=m {
        curr[0] = i;
        for j in 1..=n {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            curr[j] = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[n]
}

/// Find the closest match from a list of candidates using Levenshtein distance.
/// Returns `Some(suggestion)` if a candidate is within `max_dist` edits.
pub fn closest_match<'a>(name: &str, candidates: impl Iterator<Item = &'a str>) -> Option<String> {
    let max_dist = match name.len() {
        0..=2 => 1,
        3..=5 => 2,
        _ => 3,
    };
    candidates
        .filter(|c| *c != name && !c.starts_with("__"))
        .map(|c| (c, levenshtein(name, c)))
        .filter(|(_, d)| *d <= max_dist)
        // Prefer smallest distance, then closest length to original, then alphabetical
        .min_by(|(a, da), (b, db)| {
            da.cmp(db)
                .then_with(|| {
                    let a_diff = (a.len() as isize - name.len() as isize).unsigned_abs();
                    let b_diff = (b.len() as isize - name.len() as isize).unsigned_abs();
                    a_diff.cmp(&b_diff)
                })
                .then_with(|| a.cmp(b))
        })
        .map(|(c, _)| c.to_string())
}
