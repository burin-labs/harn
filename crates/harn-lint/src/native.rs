//! C-compatible ABI for native lint rule libraries.
//!
//! Native rules are the trusted-code escape hatch for rules that need compiled
//! Rust. A dynamic library exports [`HARN_NATIVE_LINT_REGISTER_SYMBOL`]; Harn
//! calls that function with a registry, the library registers one or more
//! [`HarnNativeRuleDescriptor`] values, and each descriptor's hooks emit
//! [`HarnNativeDiagnostic`] values through a synchronous sink.
//!
//! The ABI copies diagnostics while the hook is running. Rule authors may pass
//! borrowed strings and stack-local fix arrays to the sink as long as they stay
//! alive until [`HarnNativeDiagnosticSink::push`] returns.

use std::ffi::c_void;

/// ABI version accepted by this Harn release.
pub const HARN_NATIVE_LINT_ABI_VERSION: u32 = 1;

/// Exported registration symbol every native lint library must define.
pub const HARN_NATIVE_LINT_REGISTER_SYMBOL: &[u8] = b"harn_native_lint_register_v1\0";

/// Native rule registration function type.
pub type HarnNativeLintRegisterFn = unsafe extern "C" fn(*mut HarnNativeRuleRegistry);

/// Borrowed UTF-8 string crossing the native lint ABI.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct HarnNativeStr {
    pub ptr: *const u8,
    pub len: usize,
}

impl HarnNativeStr {
    /// Empty borrowed string.
    pub const fn empty() -> Self {
        Self {
            ptr: std::ptr::null(),
            len: 0,
        }
    }

    /// Borrow a Rust string for the duration of the current ABI call.
    pub fn borrowed(value: &str) -> Self {
        Self {
            ptr: value.as_ptr(),
            len: value.len(),
        }
    }

    /// Interpret this borrowed value as UTF-8.
    ///
    /// # Safety
    ///
    /// `ptr..ptr+len` must be readable for the returned lifetime.
    pub unsafe fn as_str(&self) -> Option<&str> {
        if self.len == 0 {
            return Some("");
        }
        if self.ptr.is_null() {
            return None;
        }
        let bytes = unsafe { std::slice::from_raw_parts(self.ptr, self.len) };
        std::str::from_utf8(bytes).ok()
    }
}

impl Default for HarnNativeStr {
    fn default() -> Self {
        Self::empty()
    }
}

/// Source span for native diagnostics and fixes.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct HarnNativeSpan {
    pub start: usize,
    pub end: usize,
    pub line: usize,
    pub column: usize,
    pub end_line: usize,
}

impl HarnNativeSpan {
    pub const fn new(start: usize, end: usize, line: usize, column: usize) -> Self {
        Self {
            start,
            end,
            line,
            column,
            end_line: line,
        }
    }
}

/// Info-level native diagnostic severity.
pub const HARN_NATIVE_SEVERITY_INFO: u32 = 0;
/// Warning-level native diagnostic severity.
pub const HARN_NATIVE_SEVERITY_WARNING: u32 = 1;
/// Error-level native diagnostic severity.
pub const HARN_NATIVE_SEVERITY_ERROR: u32 = 2;

/// One machine-applicable native fix edit.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct HarnNativeFixEdit {
    pub span: HarnNativeSpan,
    pub replacement: HarnNativeStr,
}

impl HarnNativeFixEdit {
    pub fn replace(span: HarnNativeSpan, replacement: &str) -> Self {
        Self {
            span,
            replacement: HarnNativeStr::borrowed(replacement),
        }
    }
}

/// One diagnostic emitted by a native lint rule.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct HarnNativeDiagnostic {
    pub message: HarnNativeStr,
    pub severity: u32,
    pub span: HarnNativeSpan,
    pub suggestion: HarnNativeStr,
    pub fixes: *const HarnNativeFixEdit,
    pub fix_count: usize,
}

impl HarnNativeDiagnostic {
    pub fn warning(message: &str, span: HarnNativeSpan) -> Self {
        Self {
            message: HarnNativeStr::borrowed(message),
            severity: HARN_NATIVE_SEVERITY_WARNING,
            span,
            suggestion: HarnNativeStr::empty(),
            fixes: std::ptr::null(),
            fix_count: 0,
        }
    }

    pub fn with_suggestion(mut self, suggestion: &str) -> Self {
        self.suggestion = HarnNativeStr::borrowed(suggestion);
        self
    }

    pub fn with_fixes(mut self, fixes: &[HarnNativeFixEdit]) -> Self {
        self.fixes = fixes.as_ptr();
        self.fix_count = fixes.len();
        self
    }
}

/// Read-only inputs passed to native rule hooks.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct HarnNativeRuleInput {
    pub source: HarnNativeStr,
    pub file_path: HarnNativeStr,
}

/// Read-only node view passed to native per-node hooks.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct HarnNativeNode {
    pub span: HarnNativeSpan,
    pub text: HarnNativeStr,
}

/// Diagnostic sink passed to native rule hooks.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct HarnNativeDiagnosticSink {
    pub data: *mut c_void,
    pub push: Option<unsafe extern "C" fn(*mut c_void, HarnNativeDiagnostic)>,
}

impl HarnNativeDiagnosticSink {
    /// Push a diagnostic into Harn's lint output.
    ///
    /// # Safety
    ///
    /// `self` must be the sink Harn passed to the current hook invocation.
    pub unsafe fn push(&self, diagnostic: HarnNativeDiagnostic) {
        if let Some(push) = self.push {
            unsafe { push(self.data, diagnostic) };
        }
    }
}

/// Rule hook over a whole source file.
pub type HarnNativeCheckProgramFn = unsafe extern "C" fn(
    user_data: *mut c_void,
    input: HarnNativeRuleInput,
    sink: HarnNativeDiagnosticSink,
);

/// Rule hook over one visited AST node.
pub type HarnNativeCheckNodeFn = unsafe extern "C" fn(
    user_data: *mut c_void,
    input: HarnNativeRuleInput,
    node: HarnNativeNode,
    sink: HarnNativeDiagnosticSink,
);

/// Optional destructor for descriptor-owned user data.
pub type HarnNativeDropUserDataFn = unsafe extern "C" fn(user_data: *mut c_void);

/// One native rule descriptor registered by a dynamic library.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct HarnNativeRuleDescriptor {
    pub abi_version: u32,
    pub id: HarnNativeStr,
    pub user_data: *mut c_void,
    pub check_program: Option<HarnNativeCheckProgramFn>,
    pub check_node: Option<HarnNativeCheckNodeFn>,
    pub finalize: Option<HarnNativeCheckProgramFn>,
    pub drop_user_data: Option<HarnNativeDropUserDataFn>,
}

impl HarnNativeRuleDescriptor {
    pub fn stateless(id: &str, check_program: HarnNativeCheckProgramFn) -> Self {
        Self {
            abi_version: HARN_NATIVE_LINT_ABI_VERSION,
            id: HarnNativeStr::borrowed(id),
            user_data: std::ptr::null_mut(),
            check_program: Some(check_program),
            check_node: None,
            finalize: None,
            drop_user_data: None,
        }
    }
}

/// Registry handed to a native library's registration function.
#[repr(C)]
pub struct HarnNativeRuleRegistry {
    pub data: *mut c_void,
    pub add_rule: Option<unsafe extern "C" fn(*mut c_void, HarnNativeRuleDescriptor)>,
}

impl HarnNativeRuleRegistry {
    /// Add one rule descriptor to the current registration.
    ///
    /// # Safety
    ///
    /// `self` must be the registry Harn passed to the registration function.
    pub unsafe fn add(&mut self, descriptor: HarnNativeRuleDescriptor) {
        if let Some(add_rule) = self.add_rule {
            unsafe { add_rule(self.data, descriptor) };
        }
    }
}
