use std::collections::{BTreeMap, BTreeSet};

use super::type_expr::TypeExpr;

/// Registry of reusable named types discovered during schema extraction.
/// Each tool-contract prompt build produces one registry; the renderer emits
/// `type X = ...;` aliases at the top, and tool signatures can reference them
/// by name to keep individual signatures short.
#[derive(Clone, Debug, Default)]
pub(crate) struct ComponentRegistry {
    /// Registered types by their resolved short name. Names are derived from
    /// the last path segment of the `$ref` (e.g. `#/components/schemas/Foo` → `Foo`).
    types: BTreeMap<String, TypeExpr>,
    /// Insertion order, so `type` aliases render in a deterministic stable order.
    order: Vec<String>,
    /// Set of names currently being resolved. Used to break cycles: if we
    /// encounter the same ref while it's still being resolved, we emit a
    /// `Ref(name)` placeholder and leave the alias definition to the outer
    /// call. Without this, a recursive schema would infinite-loop.
    in_progress: BTreeSet<String>,
}

impl ComponentRegistry {
    pub(super) fn register(&mut self, name: String, ty: TypeExpr) {
        if !self.types.contains_key(&name) {
            self.order.push(name.clone());
        }
        self.types.insert(name, ty);
    }

    pub(super) fn contains(&self, name: &str) -> bool {
        self.types.contains_key(name)
    }

    pub(super) fn is_in_progress(&self, name: &str) -> bool {
        self.in_progress.contains(name)
    }

    pub(super) fn begin_resolution(&mut self, name: &str) {
        self.in_progress.insert(name.to_string());
    }

    pub(super) fn finish_resolution(&mut self, name: &str) {
        self.in_progress.remove(name);
    }

    /// Render all registered types as `type Name = Expr;` lines in insertion
    /// order. Returns an empty string when the registry is empty.
    pub(crate) fn render_aliases(&self) -> String {
        if self.order.is_empty() {
            return String::new();
        }
        let mut out = String::new();
        for name in &self.order {
            if let Some(ty) = self.types.get(name) {
                out.push_str(&format!("type {} = {};\n", name, ty.render()));
            }
        }
        out
    }
}

/// Extract the short name from a JSON Pointer `$ref`. Supports common shapes:
/// `#/components/schemas/Foo`, `#/definitions/Foo`, and Harn-native
/// `types/Foo` / `#/types/Foo`. Returns None if we cannot find a name-like tail.
pub(super) fn ref_name_from_pointer(pointer: &str) -> Option<String> {
    let stripped = pointer.trim_start_matches('#').trim_start_matches('/');
    let last = stripped.rsplit('/').next()?;
    if last.is_empty() {
        None
    } else {
        Some(last.to_string())
    }
}

/// Resolve a JSON Pointer `$ref` against a root schema document. Supports
/// fragments like `#/components/schemas/Foo` by walking each path segment.
pub(super) fn resolve_json_ref<'a>(
    root: &'a serde_json::Value,
    pointer: &str,
) -> Option<&'a serde_json::Value> {
    let stripped = pointer.trim_start_matches('#').trim_start_matches('/');
    if stripped.is_empty() {
        return Some(root);
    }
    let mut current = root;
    for segment in stripped.split('/') {
        let decoded = segment.replace("~1", "/").replace("~0", "~");
        current = match current {
            serde_json::Value::Object(obj) => obj.get(&decoded)?,
            serde_json::Value::Array(arr) => {
                let idx: usize = decoded.parse().ok()?;
                arr.get(idx)?
            }
            _ => return None,
        };
    }
    Some(current)
}
