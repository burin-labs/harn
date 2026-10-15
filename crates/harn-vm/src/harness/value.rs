pub use crate::value::VmHarness;

/// Recover native authority, refusing a handle from another runtime backend.
pub fn native_harness(handle: &VmHarness) -> Result<&HarnessInner, crate::VmError> {
    handle.host_state().ok_or_else(|| {
        crate::VmError::TypeError(
            "Harness handle belongs to a different runtime backend".to_string(),
        )
    })
}

pub(crate) fn context_fixtures(
    ctx: &crate::vm::AsyncBuiltinCtx,
) -> Result<Option<Arc<CapabilityFixtureState>>, crate::VmError> {
    ctx.child_vm()
        .harness()
        .map(native_harness)
        .transpose()
        .map(|inner| inner.map(HarnessInner::fixtures_arc))
}

pub(crate) fn context_fixture(
    ctx: Option<&crate::vm::AsyncBuiltinCtx>,
    capability: harn_builtin_meta::CapabilityId,
    method: &str,
    args: &[crate::VmValue],
) -> Result<Option<crate::VmValue>, crate::VmError> {
    let Some(ctx) = ctx else { return Ok(None) };
    context_fixtures(ctx)?
        .and_then(|fixtures| fixtures.dispatch(capability, method, args))
        .transpose()
}

impl From<Harness> for VmHarness {
    fn from(harness: Harness) -> Self {
        Self::from_host(harness.inner)
    }
}

/// Native dispatch validates the backend once, then borrows it for the call.
/// This view neither allocates nor increments the backing state's reference count.
pub(crate) struct NativeVmHarness<'a> {
    handle: &'a VmHarness,
    inner: &'a HarnessInner,
}

impl<'a> NativeVmHarness<'a> {
    pub(crate) fn from_handle(handle: &'a VmHarness) -> Result<Self, crate::VmError> {
        let inner = native_harness(handle)?;
        Ok(Self { handle, inner })
    }

    pub(crate) fn inner(&self) -> &HarnessInner {
        self.inner
    }

    pub(crate) fn clone_inner(&self) -> Arc<HarnessInner> {
        self.handle
            .host_state_arc()
            .expect("validated native harness backend")
    }
}

impl std::ops::Deref for NativeVmHarness<'_> {
    type Target = VmHarness;

    fn deref(&self) -> &Self::Target {
        self.handle
    }
}
