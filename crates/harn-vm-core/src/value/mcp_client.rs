use std::any::Any;
use std::sync::Arc;

use super::VmHostObject;

/// A named MCP connection carried by the interpreter. Its runtime owns transport state.
#[derive(Clone)]
pub struct VmMcpClientHandle {
    pub name: String,
    state: VmHostObject,
}

impl VmMcpClientHandle {
    pub fn from_host<T: Any + Send + Sync>(name: String, state: Arc<T>) -> Self {
        Self {
            name,
            state: VmHostObject::from_arc(state),
        }
    }

    pub fn host_state<T: Any + Send + Sync>(&self) -> Option<Arc<T>> {
        self.state.downcast()
    }
}

impl std::fmt::Debug for VmMcpClientHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "McpClient({})", self.name)
    }
}
