use std::any::Any;
use std::sync::Arc;

/// Shared host state carried by a VM value without exposing its native backend.
/// Script code cannot construct or inspect this payload.
#[derive(Clone)]
pub struct VmHostObject(Arc<dyn Any + Send + Sync>);

impl VmHostObject {
    pub fn from_arc<T: Any + Send + Sync>(state: Arc<T>) -> Self {
        Self(state)
    }

    pub fn get<T: Any + Send + Sync>(&self) -> Option<&T> {
        self.0.downcast_ref()
    }

    pub fn downcast<T: Any + Send + Sync>(&self) -> Option<Arc<T>> {
        Arc::clone(&self.0).downcast().ok()
    }

    pub fn ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    pub(super) fn identity_ptr(&self) -> *const () {
        Arc::as_ptr(&self.0).cast::<()>()
    }
}
