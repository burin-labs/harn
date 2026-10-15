pub use crate::value::VmHarness;

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
        let inner = handle.host_state().ok_or_else(|| {
            crate::VmError::TypeError(
                "Harness handle belongs to a different runtime backend".to_string(),
            )
        })?;
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
