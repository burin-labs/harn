use std::any::Any;
use std::sync::Arc;

use harn_builtin_meta::HarnessKind;

use super::VmHostObject;

/// A typed capability handle whose backing state belongs to its runtime.
#[derive(Clone)]
pub struct VmHarness {
    state: VmHostObject,
    kind: HarnessKind,
}

impl VmHarness {
    /// Only a host can mint a root handle. Sub-handles retain the same authority.
    pub fn from_host<T: Any + Send + Sync>(state: Arc<T>) -> Self {
        Self {
            state: VmHostObject::from_arc(state),
            kind: HarnessKind::Root,
        }
    }

    pub fn host_state<T: Any + Send + Sync>(&self) -> Option<&T> {
        self.state.get()
    }

    pub fn host_state_arc<T: Any + Send + Sync>(&self) -> Option<Arc<T>> {
        self.state.downcast()
    }

    pub fn kind(&self) -> HarnessKind {
        self.kind
    }

    pub fn type_name(&self) -> &'static str {
        self.kind.type_name()
    }

    /// Whether both handles derive their authority from the same root owner.
    pub fn shares_owner(&self, other: &Self) -> bool {
        self.state.ptr_eq(&other.state)
    }

    pub fn sub_handle(&self, field: &str) -> Option<Self> {
        self.sub_handle_kind(HarnessKind::from_field_name(field)?)
    }

    pub(crate) fn sub_handle_kind(&self, kind: HarnessKind) -> Option<Self> {
        if self.kind != HarnessKind::Root || kind == HarnessKind::Root {
            return None;
        }
        Some(Self {
            state: self.state.clone(),
            kind,
        })
    }
}

impl std::fmt::Debug for VmHarness {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VmHarness")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}
