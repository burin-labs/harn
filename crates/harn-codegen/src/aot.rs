//! Ahead-of-time object-file backend.
//!
//! Emits a relocatable native object (ELF/Mach-O/COFF for the host target)
//! exporting the compiled function under a stable C symbol. The object can be
//! linked into a normal executable or shared library with any system linker —
//! this is the "compile Harn to a binary" path.

use cranelift_codegen::settings::{self, Configurable};
use cranelift_module::default_libcall_names;
use cranelift_object::{ObjectBuilder, ObjectModule};

use crate::error::CodegenError;
use crate::lower::define_scalar_function;
use crate::symbol_name;
use crate::verify::ScalarFunction;

/// A compiled object file plus the symbol the function is exported under.
#[derive(Debug, Clone)]
pub struct ObjectArtifact {
    /// The exported C symbol name (derived from the function name).
    pub symbol: String,
    /// The encoded object-file bytes for the host target.
    pub bytes: Vec<u8>,
}

/// Compile `sf` to a host-target object file.
///
/// # Errors
///
/// Returns [`CodegenError::Backend`] if the host ISA is unavailable or
/// Cranelift fails to compile or encode the object.
pub fn emit_object(sf: &ScalarFunction) -> Result<ObjectArtifact, CodegenError> {
    let mut flags = settings::builder();
    // Relocatable, position-independent code is the right default for a `.o`
    // that a linker will later place into a PIE or shared object.
    flags
        .set("is_pic", "true")
        .map_err(|e| CodegenError::backend(e.to_string()))?;
    let _ = flags.set("opt_level", "speed");

    let isa_builder =
        cranelift_native::builder().map_err(|e| CodegenError::backend(e.to_string()))?;
    let isa = isa_builder
        .finish(settings::Flags::new(flags))
        .map_err(|e| CodegenError::backend(e.to_string()))?;

    let symbol = symbol_name(&sf.name);
    let builder = ObjectBuilder::new(isa, symbol.clone(), default_libcall_names())
        .map_err(|e| CodegenError::backend(e.to_string()))?;
    let mut module = ObjectModule::new(builder);

    define_scalar_function(&mut module, sf, &symbol)?;

    let product = module.finish();
    let bytes = product
        .emit()
        .map_err(|e| CodegenError::backend(e.to_string()))?;

    Ok(ObjectArtifact { symbol, bytes })
}
