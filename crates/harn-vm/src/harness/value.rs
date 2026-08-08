/// Compact `VmValue` payload for a `Harness` or any of its sub-handles.
///
/// All handle variants share one `Arc<HarnessInner>`; `kind` discriminates the
/// surface the VM exposes for property access and method dispatch.
#[derive(Clone)]
pub struct VmHarness {
    inner: Arc<HarnessInner>,
    kind: HarnessKind,
}

impl VmHarness {
    pub fn kind(&self) -> HarnessKind {
        self.kind
    }

    pub fn type_name(&self) -> &'static str {
        self.kind.type_name()
    }

    pub fn inner(&self) -> &Arc<HarnessInner> {
        &self.inner
    }

    /// Get the sub-handle reached by a field name (`stdio`, `clock`, etc.).
    /// Returns `None` when the receiver is itself a sub-handle or the field
    /// is unknown.
    pub fn sub_handle(&self, field: &str) -> Option<VmHarness> {
        if self.kind != HarnessKind::Root {
            return None;
        }
        let kind = HarnessKind::from_field_name(field)?;
        self.sub_handle_kind(kind)
    }

    pub(crate) fn sub_handle_kind(&self, kind: HarnessKind) -> Option<VmHarness> {
        if self.kind != HarnessKind::Root || kind == HarnessKind::Root {
            return None;
        }
        Some(VmHarness {
            inner: Arc::clone(&self.inner),
            kind,
        })
    }
}

impl fmt::Debug for VmHarness {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VmHarness")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}
