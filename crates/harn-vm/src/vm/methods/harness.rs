//! Method dispatch for the `Harness` capability handle and its six
//! sub-handles. E4.1 ships the stdio surface end-to-end (`println`,
//! `eprintln`) so the new entrypoint convention has a working
//! hello-world; subsequent tickets (E4.2-E4.4) replace the stub bodies
//! on the other sub-handles with real implementations.

use std::time::Duration;

use crate::harness::{HarnessKind, VmHarness};
use crate::stdlib::io::{write_stderr, write_stdout};
use crate::value::{VmError, VmValue};

impl crate::vm::Vm {
    pub(super) async fn call_harness_method(
        &mut self,
        handle: &VmHarness,
        method: &str,
        args: &[VmValue],
    ) -> Result<VmValue, VmError> {
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
                let ms = args
                    .first()
                    .and_then(|v| match v {
                        VmValue::Int(n) => Some(*n),
                        VmValue::Duration(ms) => Some(*ms),
                        _ => None,
                    })
                    .ok_or_else(|| {
                        VmError::TypeError(
                            "HarnessClock.sleep_ms expects an int or duration argument".into(),
                        )
                    })?;
                if ms > 0 {
                    clock.sleep(Duration::from_millis(ms as u64)).await;
                }
                Ok(VmValue::Nil)
            }
            _ => Err(method_unsupported(handle, method)),
        }
    }
}

fn method_unsupported(handle: &VmHarness, method: &str) -> VmError {
    VmError::TypeError(format!(
        "value of type {} has no method `{method}`",
        handle.type_name()
    ))
}
