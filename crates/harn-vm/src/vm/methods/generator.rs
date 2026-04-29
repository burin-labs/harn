use std::collections::BTreeMap;
use std::rc::Rc;

use crate::value::{VmError, VmGenerator, VmStream, VmValue};

impl crate::vm::Vm {
    pub(super) async fn call_generator_method(
        &mut self,
        gen: &VmGenerator,
        method: &str,
    ) -> Result<VmValue, VmError> {
        match method {
            "next" => {
                if gen.done.get() {
                    let mut dict = BTreeMap::new();
                    dict.insert("value".to_string(), VmValue::Nil);
                    dict.insert("done".to_string(), VmValue::Bool(true));
                    Ok(VmValue::Dict(Rc::new(dict)))
                } else {
                    let rx = gen.receiver.clone();
                    let mut guard = rx.lock().await;
                    match guard.recv().await {
                        Some(Ok(val)) => {
                            let mut dict = BTreeMap::new();
                            dict.insert("done".to_string(), VmValue::Bool(false));
                            dict.insert("value".to_string(), val);
                            Ok(VmValue::Dict(Rc::new(dict)))
                        }
                        Some(Err(error)) => {
                            gen.done.set(true);
                            Err(error)
                        }
                        None => {
                            gen.done.set(true);
                            let mut dict = BTreeMap::new();
                            dict.insert("value".to_string(), VmValue::Nil);
                            dict.insert("done".to_string(), VmValue::Bool(true));
                            Ok(VmValue::Dict(Rc::new(dict)))
                        }
                    }
                }
            }
            _ => Ok(VmValue::Nil),
        }
    }

    pub(super) async fn call_stream_method(
        &mut self,
        stream: &VmStream,
        method: &str,
    ) -> Result<VmValue, VmError> {
        match method {
            "next" => {
                if stream.done.get() {
                    let mut dict = BTreeMap::new();
                    dict.insert("value".to_string(), VmValue::Nil);
                    dict.insert("done".to_string(), VmValue::Bool(true));
                    Ok(VmValue::Dict(Rc::new(dict)))
                } else {
                    let rx = stream.receiver.clone();
                    let mut guard = rx.lock().await;
                    match guard.recv().await {
                        Some(Ok(val)) => {
                            let mut dict = BTreeMap::new();
                            dict.insert("done".to_string(), VmValue::Bool(false));
                            dict.insert("value".to_string(), val);
                            Ok(VmValue::Dict(Rc::new(dict)))
                        }
                        Some(Err(error)) => {
                            stream.done.set(true);
                            Err(error)
                        }
                        None => {
                            stream.done.set(true);
                            let mut dict = BTreeMap::new();
                            dict.insert("value".to_string(), VmValue::Nil);
                            dict.insert("done".to_string(), VmValue::Bool(true));
                            Ok(VmValue::Dict(Rc::new(dict)))
                        }
                    }
                }
            }
            _ => Ok(VmValue::Nil),
        }
    }
}
