use std::ffi::c_void;

const ABI_VERSION: u32 = 1;
const SEVERITY_WARNING: u32 = 1;
const MARKER: &str = "NATIVE_TODO";
const REPLACEMENT: &str = "NATIVE_DONE";

#[repr(C)]
#[derive(Clone, Copy)]
pub struct NativeStr {
    ptr: *const u8,
    len: usize,
}

impl NativeStr {
    fn borrowed(value: &str) -> Self {
        Self {
            ptr: value.as_ptr(),
            len: value.len(),
        }
    }

    unsafe fn as_str(&self) -> Option<&str> {
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

#[repr(C)]
#[derive(Clone, Copy)]
pub struct NativeSpan {
    start: usize,
    end: usize,
    line: usize,
    column: usize,
    end_line: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct NativeFixEdit {
    span: NativeSpan,
    replacement: NativeStr,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct NativeDiagnostic {
    message: NativeStr,
    severity: u32,
    span: NativeSpan,
    suggestion: NativeStr,
    fixes: *const NativeFixEdit,
    fix_count: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct NativeRuleInput {
    source: NativeStr,
    file_path: NativeStr,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct NativeNode {
    span: NativeSpan,
    text: NativeStr,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct NativeDiagnosticSink {
    data: *mut c_void,
    push: Option<unsafe extern "C" fn(*mut c_void, NativeDiagnostic)>,
}

impl NativeDiagnosticSink {
    unsafe fn push(&self, diagnostic: NativeDiagnostic) {
        if let Some(push) = self.push {
            unsafe { push(self.data, diagnostic) };
        }
    }
}

type CheckProgram =
    unsafe extern "C" fn(user_data: *mut c_void, input: NativeRuleInput, sink: NativeDiagnosticSink);
type CheckNode = unsafe extern "C" fn(
    user_data: *mut c_void,
    input: NativeRuleInput,
    node: NativeNode,
    sink: NativeDiagnosticSink,
);
type DropUserData = unsafe extern "C" fn(user_data: *mut c_void);

#[repr(C)]
#[derive(Clone, Copy)]
pub struct NativeRuleDescriptor {
    abi_version: u32,
    id: NativeStr,
    user_data: *mut c_void,
    check_program: Option<CheckProgram>,
    check_node: Option<CheckNode>,
    finalize: Option<CheckProgram>,
    drop_user_data: Option<DropUserData>,
}

#[repr(C)]
pub struct NativeRuleRegistry {
    data: *mut c_void,
    add_rule: Option<unsafe extern "C" fn(*mut c_void, NativeRuleDescriptor)>,
}

impl NativeRuleRegistry {
    unsafe fn add(&mut self, descriptor: NativeRuleDescriptor) {
        if let Some(add_rule) = self.add_rule {
            unsafe { add_rule(self.data, descriptor) };
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn harn_native_lint_register_v1(registry: *mut NativeRuleRegistry) {
    let Some(registry) = (unsafe { registry.as_mut() }) else {
        return;
    };
    unsafe {
        registry.add(NativeRuleDescriptor {
            abi_version: ABI_VERSION,
            id: NativeStr::borrowed("native-no-todo"),
            user_data: std::ptr::null_mut(),
            check_program: Some(check_program),
            check_node: Some(check_node),
            finalize: None,
            drop_user_data: None,
        });
    }
}

unsafe extern "C" fn check_program(
    _user_data: *mut c_void,
    input: NativeRuleInput,
    sink: NativeDiagnosticSink,
) {
    let Some(source) = (unsafe { input.source.as_str() }) else {
        return;
    };
    for (start, _) in source.match_indices(MARKER) {
        let span = span_for(source, start, start + MARKER.len());
        let fixes = [NativeFixEdit {
            span,
            replacement: NativeStr::borrowed(REPLACEMENT),
        }];
        let diagnostic = NativeDiagnostic {
            message: NativeStr::borrowed("native rule markers must be resolved"),
            severity: SEVERITY_WARNING,
            span,
            suggestion: NativeStr::borrowed("replace NATIVE_TODO with NATIVE_DONE"),
            fixes: fixes.as_ptr(),
            fix_count: fixes.len(),
        };
        unsafe { sink.push(diagnostic) };
    }
}

unsafe extern "C" fn check_node(
    _user_data: *mut c_void,
    _input: NativeRuleInput,
    node: NativeNode,
    sink: NativeDiagnosticSink,
) {
    let Some(text) = (unsafe { node.text.as_str() }) else {
        return;
    };
    if text.trim() != "return 0" {
        return;
    }
    let diagnostic = NativeDiagnostic {
        message: NativeStr::borrowed("native node hook saw return 0"),
        severity: SEVERITY_WARNING,
        span: node.span,
        suggestion: NativeStr::borrowed("node hook diagnostics use the same sink"),
        fixes: std::ptr::null(),
        fix_count: 0,
    };
    unsafe { sink.push(diagnostic) };
}

fn span_for(source: &str, start: usize, end: usize) -> NativeSpan {
    let mut line = 1usize;
    let mut line_start = 0usize;
    for (offset, ch) in source.char_indices() {
        if offset >= start {
            break;
        }
        if ch == '\n' {
            line += 1;
            line_start = offset + 1;
        }
    }
    NativeSpan {
        start,
        end,
        line,
        column: start.saturating_sub(line_start) + 1,
        end_line: line,
    }
}
