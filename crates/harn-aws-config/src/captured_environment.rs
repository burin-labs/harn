//! One redacted environment reader shared by the captured SDK chain.

use std::collections::HashMap;
use std::env::VarError;
use std::fmt;
use std::ops::Deref;
use std::sync::Arc;

#[derive(Clone)]
pub(crate) struct Env {
    inner: aws_types::os_shim_internal::Env,
    snapshot: Option<Arc<HashMap<String, String>>>,
}

impl fmt::Debug for Env {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Env")
            .field("captured", &self.snapshot.is_some())
            .finish()
    }
}

impl Default for Env {
    fn default() -> Self {
        Self::real()
    }
}

impl Env {
    pub(crate) fn real() -> Self {
        Self {
            inner: aws_types::os_shim_internal::Env::real(),
            snapshot: None,
        }
    }

    pub(crate) fn get(&self, name: &str) -> Result<String, VarError> {
        self.inner.get(name)
    }

    pub(crate) fn snapshot(&self) -> Option<Arc<HashMap<String, String>>> {
        self.snapshot.clone()
    }

    #[cfg(test)]
    pub(crate) fn from_slice(values: &[(&str, &str)]) -> Self {
        Self::from(
            values
                .iter()
                .map(|(name, value)| (name.to_string(), value.to_string()))
                .collect::<HashMap<_, _>>(),
        )
    }
}

impl From<HashMap<String, String>> for Env {
    fn from(snapshot: HashMap<String, String>) -> Self {
        Self {
            inner: aws_types::os_shim_internal::Env::from(snapshot.clone()),
            snapshot: Some(Arc::new(snapshot)),
        }
    }
}

// Preserve the upstream ConfigLoader's test-util environment interface.
impl From<aws_types::os_shim_internal::Env> for Env {
    fn from(inner: aws_types::os_shim_internal::Env) -> Self {
        Self {
            inner,
            snapshot: None,
        }
    }
}

// Preserve the public profile parser's upstream environment interface.
impl Deref for Env {
    type Target = aws_types::os_shim_internal::Env;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}
