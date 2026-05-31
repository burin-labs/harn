use std::collections::BTreeMap;

use crate::value::{VmError, VmGenerator, VmStream, VmValue};

impl crate::vm::Vm {
    pub(super) async fn call_generator_method(
        &mut self,
        gen: &VmGenerator,
        method: &str,
    ) -> Result<VmValue, VmError> {
        match method {
            "next" => {
                if gen.is_done() {
                    let mut dict = BTreeMap::new();
                    dict.insert("value".to_string(), VmValue::Nil);
                    dict.insert("done".to_string(), VmValue::Bool(true));
                    Ok(VmValue::Dict(std::sync::Arc::new(dict)))
                } else {
                    let rx = gen.receiver.clone();
                    let mut guard = rx.lock().await;
                    match guard.recv().await {
                        Some(Ok(val)) => {
                            let mut dict = BTreeMap::new();
                            dict.insert("done".to_string(), VmValue::Bool(false));
                            dict.insert("value".to_string(), val);
                            Ok(VmValue::Dict(std::sync::Arc::new(dict)))
                        }
                        Some(Err(error)) => {
                            gen.mark_done();
                            Err(error)
                        }
                        None => {
                            gen.mark_done();
                            let mut dict = BTreeMap::new();
                            dict.insert("value".to_string(), VmValue::Nil);
                            dict.insert("done".to_string(), VmValue::Bool(true));
                            Ok(VmValue::Dict(std::sync::Arc::new(dict)))
                        }
                    }
                }
            }
            _ => Err(VmError::Runtime(format!(
                "Generator has no method `{method}`"
            ))),
        }
    }

    pub(super) async fn call_stream_method(
        &mut self,
        stream: &VmStream,
        method: &str,
    ) -> Result<VmValue, VmError> {
        match method {
            "next" => {
                if stream.is_done() {
                    let mut dict = BTreeMap::new();
                    dict.insert("value".to_string(), VmValue::Nil);
                    dict.insert("done".to_string(), VmValue::Bool(true));
                    Ok(VmValue::Dict(std::sync::Arc::new(dict)))
                } else {
                    let rx = stream.receiver.clone();
                    let mut guard = rx.lock().await;
                    match guard.recv().await {
                        Some(Ok(val)) => {
                            let mut dict = BTreeMap::new();
                            dict.insert("done".to_string(), VmValue::Bool(false));
                            dict.insert("value".to_string(), val);
                            Ok(VmValue::Dict(std::sync::Arc::new(dict)))
                        }
                        Some(Err(error)) => {
                            stream.mark_done();
                            Err(error)
                        }
                        None => {
                            stream.mark_done();
                            let mut dict = BTreeMap::new();
                            dict.insert("value".to_string(), VmValue::Nil);
                            dict.insert("done".to_string(), VmValue::Bool(true));
                            Ok(VmValue::Dict(std::sync::Arc::new(dict)))
                        }
                    }
                }
            }
            _ => Err(VmError::Runtime(format!("Stream has no method `{method}`"))),
        }
    }
}
