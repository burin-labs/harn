//! In-process JIT backend.
//!
//! Compiles a [`ScalarFunction`] to native machine code in memory and hands
//! back a [`NativeFunction`] that can be called with marshalled scalar
//! arguments. This is the path that could shave interpreter dispatch overhead
//! off a hot, pure-compute Harn kernel.

use std::mem;

use cranelift_codegen::settings::{self, Configurable};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::default_libcall_names;

use crate::error::{CodegenError, NativeTrap};
use crate::lower::{define_scalar_function, status};
use crate::outcome::{DeoptReason, NativeOutcome};
use crate::symbol_name;
use crate::value::{ScalarType, ScalarValue};
use crate::verify::ScalarFunction;

/// The raw uniform ABI of every compiled function. The third pointer receives a
/// status code (see [`status`]): `0` = result in `*ret`, non-zero = trap/deopt.
type RawFn = extern "C" fn(args: *const u64, ret: *mut u64, status: *mut u8);

/// A JIT-compiled scalar function with live executable code.
///
/// The owned [`JITModule`] backs the code mapping; dropping the
/// `NativeFunction` unmaps it, so the function pointer must not outlive this
/// value. Not `Send`/`Sync`: call it from the thread that compiled it.
pub struct NativeFunction {
    // Field order matters: `module` owns the executable mapping that
    // `func_ptr` points into, and struct fields drop in declaration order.
    func_ptr: *const u8,
    params: Vec<ScalarType>,
    ret: ScalarType,
    _module: JITModule,
}

impl NativeFunction {
    /// The parameter types this function expects, in order.
    #[must_use]
    pub fn params(&self) -> &[ScalarType] {
        &self.params
    }

    /// The function's return type.
    #[must_use]
    pub fn ret(&self) -> ScalarType {
        self.ret
    }

    /// Call the compiled function.
    ///
    /// # Panics
    ///
    /// Panics if `args` does not match the function's parameter arity and
    /// types — that is a caller bug, analogous to calling a Rust `fn` with the
    /// wrong signature.
    ///
    /// Returns [`NativeOutcome::Value`] (bit-identical to the Harn VM) on the
    /// normal path, or [`NativeOutcome::Deopt`] when an integer operation
    /// overflowed and the VM would promote to `float` (re-run on the VM for the
    /// true value).
    ///
    /// # Errors
    ///
    /// Returns [`NativeTrap`] when the code raised a runtime trap (integer
    /// divide by zero) — which the VM raises too.
    pub fn call(&self, args: &[ScalarValue]) -> Result<NativeOutcome, NativeTrap> {
        assert_eq!(
            args.len(),
            self.params.len(),
            "argument count mismatch: expected {}, got {}",
            self.params.len(),
            args.len()
        );
        for (idx, (arg, expected)) in args.iter().zip(&self.params).enumerate() {
            assert_eq!(
                arg.ty(),
                *expected,
                "argument {idx} type mismatch: expected {expected}, got {}",
                arg.ty()
            );
        }

        // A non-empty backing buffer keeps `as_ptr` valid even for nullary
        // functions (which never dereference it).
        let mut arg_bits: Vec<u64> = args.iter().map(|v| v.to_bits()).collect();
        if arg_bits.is_empty() {
            arg_bits.push(0);
        }
        let mut ret_bits: u64 = 0;
        let mut status_code: u8 = status::OK;

        // SAFETY: `func_ptr` was produced by Cranelift for the uniform ABI
        // declared in `lower`, and the backing module is kept alive by `self`.
        let raw: RawFn = unsafe { mem::transmute::<*const u8, RawFn>(self.func_ptr) };
        raw(arg_bits.as_ptr(), &raw mut ret_bits, &raw mut status_code);

        match status_code {
            status::OK => Ok(NativeOutcome::Value(ScalarValue::from_bits(
                self.ret, ret_bits,
            ))),
            status::DIVIDE_BY_ZERO => Err(NativeTrap::DivideByZero),
            status::INTEGER_OVERFLOW => Ok(NativeOutcome::Deopt(DeoptReason::IntegerOverflow)),
            other => unreachable!("native code returned unknown status code {other}"),
        }
    }
}

/// Compile `sf` to native code with the in-process JIT.
///
/// # Errors
///
/// Returns [`CodegenError::Backend`] if the host ISA is unavailable or
/// Cranelift fails to compile the lowered IR.
pub fn compile(sf: &ScalarFunction) -> Result<NativeFunction, CodegenError> {
    let mut flags = settings::builder();
    // The JIT maps code into this process; non-PIC, in-process libcalls.
    set_flag(&mut flags, "use_colocated_libcalls", "false")?;
    set_flag(&mut flags, "is_pic", "false")?;
    let _ = flags.set("opt_level", "speed");

    let isa_builder =
        cranelift_native::builder().map_err(|e| CodegenError::backend(e.to_string()))?;
    let isa = isa_builder
        .finish(settings::Flags::new(flags))
        .map_err(|e| CodegenError::backend(e.to_string()))?;

    let builder = JITBuilder::with_isa(isa, default_libcall_names());
    let mut module = JITModule::new(builder);

    let id = define_scalar_function(&mut module, sf, &symbol_name(&sf.name))?;
    module
        .finalize_definitions()
        .map_err(|e| CodegenError::backend(e.to_string()))?;
    let func_ptr = module.get_finalized_function(id);

    Ok(NativeFunction {
        func_ptr,
        params: sf.params.clone(),
        ret: sf.ret,
        _module: module,
    })
}

fn set_flag(builder: &mut settings::Builder, name: &str, value: &str) -> Result<(), CodegenError> {
    builder
        .set(name, value)
        .map_err(|e| CodegenError::backend(format!("setting `{name}`: {e}")))
}
