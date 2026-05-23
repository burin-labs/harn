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

    pub(super) fn mark_ref(&mut self) {
        self.saw_ref = true;
    }

    pub(super) fn saw_ref(&self) -> bool {
        self.saw_ref
    }
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
