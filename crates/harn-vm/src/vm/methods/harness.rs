//! Method dispatch for the `Harness` capability handle and its six
//! sub-handles. The stdio and clock slices are wired end-to-end; subsequent
//! tickets replace the stub bodies on the remaining sub-handles with real
//! implementations.

use std::time::Duration;

use crate::harness::{vm_string, HarnessKind, HarnessMode, VmHarness};
use crate::stdlib::io::{
    prompt_user_value, read_line_legacy_value, read_line_structured_value, write_stderr,
    write_stdout,
};
use crate::value::{ErrorCategory, VmError, VmValue};

impl crate::vm::Vm {
    pub(super) async fn call_harness_method(
        &mut self,
        handle: &VmHarness,
        method: &str,
        args: &[VmValue],
    ) -> Result<VmValue, VmError> {
        if let HarnessMode::Null(state) = handle.inner().mode() {
            state.record_deny(handle.kind(), method, args);
            return Err(VmError::CategorizedError {
                message: format!("NullHarness denied {}::{method}", handle.kind().type_name()),
                category: ErrorCategory::ToolRejected,
            });
        }
        if matches!(handle.inner().mode(), HarnessMode::Mock(_)) {
            return self.call_mock_harness_method(handle, method, args).await;
        }
        match handle.kind() {
            HarnessKind::Root => Err(method_unsupported(handle, method)),
            HarnessKind::Stdio => self.call_harness_stdio_method(handle, method, args),
            HarnessKind::Clock => self.call_harness_clock_method(handle, method, args).await,
            HarnessKind::Fs | HarnessKind::Env | HarnessKind::Random | HarnessKind::Net => {
                Err(VmError::TypeError(format!(
                "{}::{method} is not yet implemented — wired by the E4.2-E4.4 migration tickets",
                handle.type_name(),
            )))
            }
        }
    }

    fn call_harness_stdio_method(
        &mut self,
        handle: &VmHarness,
        method: &str,
        args: &[VmValue],
    ) -> Result<VmValue, VmError> {
        match method {
            "println" => {
                let msg = args.first().map(|a| a.display()).unwrap_or_default();
                write_stdout(&mut self.output, &format!("{msg}\n"));
                Ok(VmValue::Nil)
            }
            "print" => {
                let msg = args.first().map(|a| a.display()).unwrap_or_default();
                write_stdout(&mut self.output, &msg);
                Ok(VmValue::Nil)
            }
            "eprintln" => {
                let msg = args.first().map(|a| a.display()).unwrap_or_default();
                write_stderr(&format!("{msg}\n"));
                Ok(VmValue::Nil)
            }
            "eprint" => {
                let msg = args.first().map(|a| a.display()).unwrap_or_default();
                write_stderr(&msg);
                Ok(VmValue::Nil)
            }
            "read_line" => {
                if args.is_empty() {
                    Ok(read_line_legacy_value())
                } else {
                    read_line_structured_value(args)
                }
            }
            "prompt" => prompt_user_value(args, &mut self.output),
            _ => Err(method_unsupported(handle, method)),
        }
    }

    async fn call_harness_clock_method(
        &mut self,
        handle: &VmHarness,
        method: &str,
        args: &[VmValue],
    ) -> Result<VmValue, VmError> {
        let clock = handle.inner().clock();
        match method {
            "now_ms" => Ok(VmValue::Int(crate::clock::now_wall_ms(clock.as_ref()))),
            "timestamp" => Ok(VmValue::Float(
                crate::clock::now_wall_ms(clock.as_ref()) as f64 / 1_000.0,
            )),
            "monotonic_ms" | "elapsed" => Ok(VmValue::Int(clock.monotonic_ms())),
            "sleep_ms" => {
                let ms = sleep_ms_arg(args)?;
                if ms > 0 {
                    clock.sleep(Duration::from_millis(ms as u64)).await;
                }
                Ok(VmValue::Nil)
            }
            _ => Err(method_unsupported(handle, method)),
        }
    }

    async fn call_mock_harness_method(
        &mut self,
        handle: &VmHarness,
        method: &str,
        args: &[VmValue],
    ) -> Result<VmValue, VmError> {
        let HarnessMode::Mock(state) = handle.inner().mode() else {
            unreachable!("mock dispatch is only called for mock harnesses");
        };
        state.record_call(handle.kind(), method, args);
        match handle.kind() {
            HarnessKind::Root => Err(method_unsupported(handle, method)),
            HarnessKind::Stdio => match method {
                "println" => {
                    let msg = args.first().map(|a| a.display()).unwrap_or_default();
                    state.push_stdio(&format!("{msg}\n"));
                    Ok(VmValue::Nil)
                }
                "print" => {
                    let msg = args.first().map(|a| a.display()).unwrap_or_default();
                    state.push_stdio(&msg);
                    Ok(VmValue::Nil)
                }
                "eprintln" | "eprint" => Ok(VmValue::Nil),
                _ => Err(method_unsupported(handle, method)),
            },
            HarnessKind::Clock => {
                let clock = handle.inner().clock();
                match method {
                    "now_ms" => Ok(VmValue::Int(crate::clock::now_wall_ms(clock.as_ref()))),
                    "timestamp" => Ok(VmValue::Float(
                        crate::clock::now_wall_ms(clock.as_ref()) as f64 / 1_000.0,
                    )),
                    "monotonic_ms" | "elapsed" => Ok(VmValue::Int(clock.monotonic_ms())),
                    "sleep_ms" => {
                        let ms = sleep_ms_arg(args)?;
                        if ms > 0 {
                            state.advance_clock(Duration::from_millis(ms as u64));
                        }
                        Ok(VmValue::Nil)
                    }
                    _ => Err(method_unsupported(handle, method)),
                }
            }
            HarnessKind::Fs => match method {
                "read_file" | "read" => {
                    let path = string_arg(args, 0, "HarnessFs.read_file")?;
                    let bytes = state
                        .fs_read(path)
                        .ok_or_else(|| VmError::CategorizedError {
                            message: format!("MockHarness has no fs_read response for {path}"),
                            category: ErrorCategory::NotFound,
                        })?;
                    Ok(VmValue::Bytes(std::rc::Rc::new(bytes.to_vec())))
                }
                "read_text" => {
                    let path = string_arg(args, 0, "HarnessFs.read_text")?;
                    let bytes = state
                        .fs_read(path)
                        .ok_or_else(|| VmError::CategorizedError {
                            message: format!("MockHarness has no fs_read response for {path}"),
                            category: ErrorCategory::NotFound,
                        })?;
                    let text = std::str::from_utf8(bytes).map_err(|error| {
                        VmError::TypeError(format!("HarnessFs.read_text: {error}"))
                    })?;
                    Ok(vm_string(text))
                }
                "exists" => {
                    let path = string_arg(args, 0, "HarnessFs.exists")?;
                    Ok(VmValue::Bool(state.fs_read(path).is_some()))
                }
                _ => Err(method_unsupported(handle, method)),
            },
            HarnessKind::Env => match method {
                "get" => {
                    let key = string_arg(args, 0, "HarnessEnv.get")?;
                    Ok(state.env_get(key).map(vm_string).unwrap_or(VmValue::Nil))
                }
                _ => Err(method_unsupported(handle, method)),
            },
            HarnessKind::Random => match method {
                "u64" | "gen_u64" => state
                    .next_random_u64()
                    .map(|value| VmValue::Int(value.min(i64::MAX as u64) as i64))
                    .ok_or_else(|| VmError::CategorizedError {
                        message: "MockHarness has no random_u64 response".to_string(),
                        category: ErrorCategory::NotFound,
                    }),
                _ => Err(method_unsupported(handle, method)),
            },
            HarnessKind::Net => match method {
                "get" | "http_get" => {
                    let url = string_arg(args, 0, "HarnessNet.get")?;
                    Ok(state.net_get(url).map(vm_string).ok_or_else(|| {
                        VmError::CategorizedError {
                            message: format!("MockHarness has no net_get response for {url}"),
                            category: ErrorCategory::NotFound,
                        }
                    })?)
                }
                _ => Err(method_unsupported(handle, method)),
            },
        }
    }
}

fn method_unsupported(handle: &VmHarness, method: &str) -> VmError {
    VmError::TypeError(format!(
        "value of type {} has no method `{method}`",
        handle.type_name()
    ))
}

fn sleep_ms_arg(args: &[VmValue]) -> Result<i64, VmError> {
    args.first()
        .and_then(|v| match v {
            VmValue::Int(n) => Some(*n),
            VmValue::Duration(ms) => Some(*ms),
            _ => None,
        })
        .ok_or_else(|| {
            VmError::TypeError("HarnessClock.sleep_ms expects an int or duration argument".into())
        })
}

fn string_arg<'a>(args: &'a [VmValue], index: usize, callee: &str) -> Result<&'a str, VmError> {
    match args.get(index) {
        Some(VmValue::String(value)) => Ok(value.as_ref()),
        Some(other) => Err(VmError::TypeError(format!(
            "{callee} expects string argument {}, got {}",
            index + 1,
            other.type_name()
        ))),
        None => Err(VmError::TypeError(format!(
            "{callee} expects string argument {}",
            index + 1
        ))),
    }
}
