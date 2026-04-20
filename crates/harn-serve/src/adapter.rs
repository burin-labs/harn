use async_trait::async_trait;

use crate::{CallRequest, CallResponse, DispatchCore, DispatchError};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdapterDescriptor {
    pub id: String,
    pub caller_shape: String,
    pub supports_streaming: bool,
    pub supports_cancel: bool,
}

impl AdapterDescriptor {
    pub fn new(id: impl Into<String>, caller_shape: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            caller_shape: caller_shape.into(),
            supports_streaming: false,
            supports_cancel: true,
        }
    }
}

#[async_trait(?Send)]
pub trait TransportAdapter: Send + Sync {
    fn descriptor(&self) -> AdapterDescriptor;

    async fn dispatch(
        &self,
        core: &DispatchCore,
        request: CallRequest,
    ) -> Result<CallResponse, DispatchError> {
        core.dispatch(request).await
    }
}
