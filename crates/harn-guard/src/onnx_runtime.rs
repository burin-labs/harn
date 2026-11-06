//! Shared ONNX environment for both local model capabilities.

use std::sync::Mutex;

use ort::environment::Environment;

use crate::error::{GuardError, Result};

static ENVIRONMENT: Mutex<Option<Environment>> = Mutex::new(None);

/// Supply the process's existing ONNX environment before loading a guard model.
///
/// Embedders that initialize ONNX themselves must register that environment so
/// guard classifiers and text encoders use the same process-wide runtime.
pub fn set_onnx_environment(environment: Environment) -> Result<()> {
    let mut current = ENVIRONMENT
        .lock()
        .map_err(|_| GuardError::Inference("ONNX environment lock poisoned".to_owned()))?;
    if current.is_some() {
        return Err(GuardError::Inference(
            "ONNX environment is already configured".to_owned(),
        ));
    }
    *current = Some(environment);
    Ok(())
}

pub(crate) fn environment() -> Result<Environment> {
    let mut current = ENVIRONMENT
        .lock()
        .map_err(|_| GuardError::Inference("ONNX environment lock poisoned".to_owned()))?;
    if let Some(environment) = current.as_ref() {
        return Ok(environment.clone());
    }
    let environment = ort::init()
        .build()
        .map_err(|error| GuardError::Inference(format!("initialize ONNX runtime: {error}")))?;
    *current = Some(environment.clone());
    Ok(environment)
}
