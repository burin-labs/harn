pub(super) const DEFAULT_SCHEMA_MAX_DEPTH: usize = 128;
pub(super) const DEFAULT_SCHEMA_MAX_REF_EXPANSIONS: usize = 256;

#[derive(Clone, Copy, Debug)]
pub(super) struct SchemaLimits {
    pub(super) max_depth: usize,
    pub(super) max_ref_expansions: usize,
}

impl Default for SchemaLimits {
    fn default() -> Self {
        Self {
            max_depth: DEFAULT_SCHEMA_MAX_DEPTH,
            max_ref_expansions: DEFAULT_SCHEMA_MAX_REF_EXPANSIONS,
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct SchemaTraversal {
    limits: SchemaLimits,
    depth: usize,
    ref_expansions: usize,
    saw_ref: bool,
}

impl SchemaTraversal {
    pub(super) fn new() -> Self {
        Self {
            limits: SchemaLimits::default(),
            depth: 0,
            ref_expansions: 0,
            saw_ref: false,
        }
    }

    pub(super) fn enter_schema(&mut self) -> Result<(), String> {
        if self.depth >= self.limits.max_depth {
            return Err(format!("schema depth exceeded ({})", self.limits.max_depth));
        }
        self.depth += 1;
        Ok(())
    }

    pub(super) fn exit_schema(&mut self) {
        self.depth = self.depth.saturating_sub(1);
    }

    pub(super) fn expand_ref(&mut self) -> Result<(), String> {
        self.ref_expansions += 1;
        if self.ref_expansions > self.limits.max_ref_expansions {
            return Err(format!(
                "schema $ref expansion limit exceeded ({})",
                self.limits.max_ref_expansions
            ));
        }
        Ok(())
    }

    /// The budget spent so far, to hand back before validating a sibling value.
    ///
    /// Prefer `with_child_ref_budget`. This pair exists for callers that hold
    /// the traversal inside a larger context they also need to pass on, where a
    /// closure would borrow that context twice.
    pub(super) fn ref_budget_mark(&self) -> usize {
        self.ref_expansions
    }

    pub(super) fn restore_ref_budget(&mut self, mark: usize) {
        self.ref_expansions = mark;
    }

    pub(super) fn mark_ref(&mut self) {
        self.saw_ref = true;
    }

    pub(super) fn saw_ref(&self) -> bool {
        self.saw_ref
    }
}

/// Validates one child value with the `$ref` budget it was given, and hands the
/// same budget to the next child.
///
/// The budget exists to bound how much work a single value can force: `all_of`
/// and `union` re-expand the *same* value against several branches, so a schema
/// that branches on every hop can blow up exponentially without ever nesting
/// deeply. That is a property of one value's subtree.
///
/// Spending it across the whole document instead made validation reject data
/// for being large. An array of 300 integers whose `items` is a `$ref` is 300
/// ordinary expansions, one per element; without this the 257th element and
/// every element after it failed with "expansion limit exceeded", naming the
/// data when nothing was wrong with it.
pub(super) fn with_child_ref_budget<T>(
    traversal: &mut SchemaTraversal,
    f: impl FnOnce(&mut SchemaTraversal) -> T,
) -> T {
    let mark = traversal.ref_expansions;
    let result = f(traversal);
    traversal.ref_expansions = mark;
    result
}

pub(super) fn with_schema_depth<T>(
    traversal: &mut SchemaTraversal,
    f: impl FnOnce(&mut SchemaTraversal) -> Result<T, String>,
) -> Result<T, String> {
    traversal.enter_schema()?;
    let result = f(traversal);
    traversal.exit_schema();
    result
}
