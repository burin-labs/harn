//! Helpers for assembling [`VmValue::Dict`] response bodies that match the
//! `schemas/tools/<method>.response.json` contracts.

use std::collections::BTreeMap;
use std::sync::Arc;

use harn_vm::VmValue;

/// Type-erased builder for the dict that becomes a tool's response.
#[derive(Default)]
pub(crate) struct ResponseBuilder {
    inner: BTreeMap<String, VmValue>,
}

impl ResponseBuilder {
    pub(crate) fn new() -> Self {
        Self {
            inner: BTreeMap::new(),
        }
    }

    pub(crate) fn str(mut self, key: &str, value: impl Into<String>) -> Self {
        self.inner
            .insert(key.to_string(), VmValue::String(Arc::from(value.into())));
        self
    }

    pub(crate) fn int(mut self, key: &str, value: i64) -> Self {
        self.inner.insert(key.to_string(), VmValue::Int(value));
        self
    }

    pub(crate) fn bool(mut self, key: &str, value: bool) -> Self {
        self.inner.insert(key.to_string(), VmValue::Bool(value));
        self
    }

    pub(crate) fn opt_str(mut self, key: &str, value: Option<impl Into<String>>) -> Self {
        match value {
            Some(v) => {
                self.inner
                    .insert(key.to_string(), VmValue::String(Arc::from(v.into())));
            }
            None => {
                self.inner.insert(key.to_string(), VmValue::Nil);
            }
        }
        self
    }

    pub(crate) fn nil(mut self, key: &str) -> Self {
        self.inner.insert(key.to_string(), VmValue::Nil);
        self
    }

    pub(crate) fn dict(mut self, key: &str, value: BTreeMap<String, VmValue>) -> Self {
        self.inner
            .insert(key.to_string(), VmValue::Dict(Arc::new(value)));
        self
    }

    pub(crate) fn list(mut self, key: &str, value: Vec<VmValue>) -> Self {
        self.inner
            .insert(key.to_string(), VmValue::List(Arc::new(value)));
        self
    }

    pub(crate) fn build(self) -> VmValue {
        VmValue::Dict(Arc::new(self.inner))
    }
}
