//! Loader and adapter for trusted native lint rule libraries.

use std::path::{Path, PathBuf};

use harn_lexer::{FixEdit, Span};
use harn_parser::{DiagnosticCode, SNode};

use crate::diagnostic::{LintDiagnostic, LintSeverity};
use crate::native::{
    HarnNativeDiagnostic, HarnNativeDiagnosticSink, HarnNativeFixEdit, HarnNativeNode,
    HarnNativeRuleDescriptor, HarnNativeRuleInput, HarnNativeRuleRegistry, HarnNativeStr,
    HARN_NATIVE_LINT_ABI_VERSION, HARN_NATIVE_LINT_REGISTER_SYMBOL, HARN_NATIVE_SEVERITY_ERROR,
    HARN_NATIVE_SEVERITY_INFO,
};
use crate::rule::{Rule, RuleCtx};

const LOADER_RULE_ID: &str = "native-rule-loader";

pub(crate) fn load_rules_from_paths(
    paths: &[PathBuf],
) -> (Vec<Box<dyn Rule>>, Vec<LintDiagnostic>) {
    let mut rules = Vec::new();
    let mut diagnostics = Vec::new();
    for path in paths {
        match load_rules_from_path(path) {
            Ok(mut loaded) => rules.append(&mut loaded),
            Err(error) => diagnostics.push(error.into_diagnostic()),
        }
    }
    (rules, diagnostics)
}

#[cfg(target_arch = "wasm32")]
fn load_rules_from_path(path: &Path) -> Result<Vec<Box<dyn Rule>>, NativeRuleLoadError> {
    Err(NativeRuleLoadError::UnsupportedTarget {
        path: path.to_path_buf(),
    })
}

#[cfg(not(target_arch = "wasm32"))]
fn load_rules_from_path(path: &Path) -> Result<Vec<Box<dyn Rule>>, NativeRuleLoadError> {
    use std::sync::Arc;

    use libloading::Library;

    let library = unsafe {
        Library::new(path).map_err(|error| NativeRuleLoadError::Open {
            path: path.to_path_buf(),
            error: error.to_string(),
        })?
    };
    let mut descriptors = Vec::new();
    {
        let register = unsafe {
            library
                .get::<crate::native::HarnNativeLintRegisterFn>(HARN_NATIVE_LINT_REGISTER_SYMBOL)
                .map_err(|error| NativeRuleLoadError::MissingEntrypoint {
                    path: path.to_path_buf(),
                    error: error.to_string(),
                })?
        };
        let mut registry = HarnNativeRuleRegistry {
            data: (&raw mut descriptors).cast(),
            add_rule: Some(collect_rule),
        };
        unsafe { register(&raw mut registry) };
    }

    if descriptors.is_empty() {
        return Err(NativeRuleLoadError::NoRules {
            path: path.to_path_buf(),
        });
    }

    let mut validated = Vec::with_capacity(descriptors.len());
    for collected in descriptors {
        match validate_descriptor(path, collected) {
            Ok(rule) => validated.push(rule),
            Err(error) => {
                for (_, descriptor) in validated {
                    drop_descriptor_data(descriptor);
                }
                return Err(error);
            }
        }
    }

    let library = Arc::new(library);
    let mut rules: Vec<Box<dyn Rule>> = Vec::with_capacity(validated.len());
    for (id, descriptor) in validated {
        rules.push(Box::new(LoadedNativeRule {
            id,
            user_data: descriptor.user_data,
            check_program: descriptor.check_program,
            check_node: descriptor.check_node,
            finalize: descriptor.finalize,
            drop_user_data: descriptor.drop_user_data,
            _library: Arc::clone(&library),
        }));
    }
    Ok(rules)
}

#[cfg(not(target_arch = "wasm32"))]
unsafe extern "C" fn collect_rule(data: *mut std::ffi::c_void, rule: HarnNativeRuleDescriptor) {
    if data.is_null() {
        return;
    }
    let rules = unsafe { &mut *data.cast::<Vec<CollectedDescriptor>>() };
    rules.push(CollectedDescriptor {
        id: native_str_to_string(rule.id),
        descriptor: rule,
    });
}

struct CollectedDescriptor {
    id: Option<String>,
    descriptor: HarnNativeRuleDescriptor,
}

fn validate_descriptor(
    path: &Path,
    collected: CollectedDescriptor,
) -> Result<(String, HarnNativeRuleDescriptor), NativeRuleLoadError> {
    let descriptor = collected.descriptor;
    if descriptor.abi_version != HARN_NATIVE_LINT_ABI_VERSION {
        drop_descriptor_data(descriptor);
        return Err(NativeRuleLoadError::UnsupportedAbi {
            path: path.to_path_buf(),
            expected: HARN_NATIVE_LINT_ABI_VERSION,
            actual: descriptor.abi_version,
        });
    }
    let id = collected.id.ok_or_else(|| {
        drop_descriptor_data(descriptor);
        NativeRuleLoadError::InvalidId {
            path: path.to_path_buf(),
        }
    })?;
    if id.trim().is_empty() {
        drop_descriptor_data(descriptor);
        return Err(NativeRuleLoadError::InvalidId {
            path: path.to_path_buf(),
        });
    }
    if descriptor.check_program.is_none()
        && descriptor.check_node.is_none()
        && descriptor.finalize.is_none()
    {
        drop_descriptor_data(descriptor);
        return Err(NativeRuleLoadError::NoHooks {
            path: path.to_path_buf(),
            id,
        });
    }
    Ok((id, descriptor))
}

fn drop_descriptor_data(descriptor: HarnNativeRuleDescriptor) {
    if let Some(drop_user_data) = descriptor.drop_user_data {
        unsafe { drop_user_data(descriptor.user_data) };
    }
}

#[cfg(not(target_arch = "wasm32"))]
struct LoadedNativeRule {
    id: String,
    user_data: *mut std::ffi::c_void,
    check_program: Option<crate::native::HarnNativeCheckProgramFn>,
    check_node: Option<crate::native::HarnNativeCheckNodeFn>,
    finalize: Option<crate::native::HarnNativeCheckProgramFn>,
    drop_user_data: Option<crate::native::HarnNativeDropUserDataFn>,
    _library: std::sync::Arc<libloading::Library>,
}

#[cfg(not(target_arch = "wasm32"))]
impl Drop for LoadedNativeRule {
    fn drop(&mut self) {
        if let Some(drop_user_data) = self.drop_user_data {
            unsafe { drop_user_data(self.user_data) };
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Rule for LoadedNativeRule {
    fn id(&self) -> &str {
        &self.id
    }

    fn visits_nodes(&self) -> bool {
        self.check_node.is_some()
    }

    fn check_program(
        &mut self,
        _program: &[SNode],
        ctx: &RuleCtx<'_>,
        out: &mut Vec<LintDiagnostic>,
    ) {
        if let Some(check_program) = self.check_program {
            self.run_hook(ctx, out, check_program);
        }
    }

    fn finalize(&mut self, ctx: &RuleCtx<'_>, out: &mut Vec<LintDiagnostic>) {
        if let Some(finalize) = self.finalize {
            self.run_hook(ctx, out, finalize);
        }
    }

    fn check_node(&mut self, node: &SNode, ctx: &RuleCtx<'_>, out: &mut Vec<LintDiagnostic>) {
        if let Some(check_node) = self.check_node {
            self.run_node_hook(node, ctx, out, check_node);
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl LoadedNativeRule {
    fn run_hook(
        &mut self,
        ctx: &RuleCtx<'_>,
        out: &mut Vec<LintDiagnostic>,
        hook: crate::native::HarnNativeCheckProgramFn,
    ) {
        let Some((input, _path)) = native_input(ctx) else {
            return;
        };
        let mut sink = NativeSink {
            rule_id: &self.id,
            out,
        };
        let sink = HarnNativeDiagnosticSink {
            data: (&raw mut sink).cast(),
            push: Some(push_native_diagnostic),
        };
        unsafe { hook(self.user_data, input, sink) };
    }

    fn run_node_hook(
        &mut self,
        node: &SNode,
        ctx: &RuleCtx<'_>,
        out: &mut Vec<LintDiagnostic>,
        hook: crate::native::HarnNativeCheckNodeFn,
    ) {
        let Some((input, _path)) = native_input(ctx) else {
            return;
        };
        let source = ctx.source.unwrap_or_default();
        let text = source
            .get(node.span.start..node.span.end)
            .unwrap_or_default();
        let native_node = HarnNativeNode {
            span: span_to_native(node.span),
            text: HarnNativeStr::borrowed(text),
        };
        let mut sink = NativeSink {
            rule_id: &self.id,
            out,
        };
        let sink = HarnNativeDiagnosticSink {
            data: (&raw mut sink).cast(),
            push: Some(push_native_diagnostic),
        };
        unsafe { hook(self.user_data, input, native_node, sink) };
    }
}

fn native_input(ctx: &RuleCtx<'_>) -> Option<(HarnNativeRuleInput, Option<String>)> {
    let source = ctx.source?;
    let path = ctx
        .file_path
        .map(|path| path.to_string_lossy().into_owned());
    let input = HarnNativeRuleInput {
        source: HarnNativeStr::borrowed(source),
        file_path: path
            .as_deref()
            .map_or_else(HarnNativeStr::empty, HarnNativeStr::borrowed),
    };
    Some((input, path))
}

struct NativeSink<'rule, 'out> {
    rule_id: &'rule str,
    out: &'out mut Vec<LintDiagnostic>,
}

unsafe extern "C" fn push_native_diagnostic(
    data: *mut std::ffi::c_void,
    diagnostic: HarnNativeDiagnostic,
) {
    if data.is_null() {
        return;
    }
    let sink = unsafe { &mut *data.cast::<NativeSink<'_, '_>>() };
    if let Some(diagnostic) = diagnostic_from_native(sink.rule_id, diagnostic) {
        sink.out.push(diagnostic);
    }
}

fn diagnostic_from_native(
    rule_id: &str,
    diagnostic: HarnNativeDiagnostic,
) -> Option<LintDiagnostic> {
    let message = native_str_to_string(diagnostic.message)?;
    if message.is_empty() {
        return None;
    }
    let span = span_from_native(diagnostic.span);
    Some(LintDiagnostic {
        code: DiagnosticCode::LintRuleEngine,
        rule: std::borrow::Cow::Owned(rule_id.to_string()),
        message,
        span,
        severity: severity_from_native(diagnostic.severity),
        suggestion: native_str_to_string(diagnostic.suggestion).filter(|s| !s.is_empty()),
        fix: fixes_from_native(diagnostic.fixes, diagnostic.fix_count),
    })
}

fn native_str_to_string(value: HarnNativeStr) -> Option<String> {
    let text = unsafe { value.as_str()? };
    Some(text.to_string())
}

fn span_from_native(span: crate::native::HarnNativeSpan) -> Span {
    Span {
        start: span.start,
        end: span.end.max(span.start),
        line: span.line.max(1),
        column: span.column.max(1),
        end_line: span.end_line.max(span.line).max(1),
    }
}

fn span_to_native(span: Span) -> crate::native::HarnNativeSpan {
    crate::native::HarnNativeSpan {
        start: span.start,
        end: span.end,
        line: span.line,
        column: span.column,
        end_line: span.end_line,
    }
}

fn severity_from_native(severity: u32) -> LintSeverity {
    match severity {
        HARN_NATIVE_SEVERITY_INFO => LintSeverity::Info,
        HARN_NATIVE_SEVERITY_ERROR => LintSeverity::Error,
        _ => LintSeverity::Warning,
    }
}

fn fixes_from_native(fixes: *const HarnNativeFixEdit, fix_count: usize) -> Option<Vec<FixEdit>> {
    if fix_count == 0 || fixes.is_null() {
        return None;
    }
    let fixes = unsafe { std::slice::from_raw_parts(fixes, fix_count) };
    let mut out = Vec::with_capacity(fixes.len());
    for fix in fixes {
        let replacement = native_str_to_string(fix.replacement)?;
        out.push(FixEdit {
            span: span_from_native(fix.span),
            replacement,
        });
    }
    Some(out)
}

enum NativeRuleLoadError {
    Open {
        path: PathBuf,
        error: String,
    },
    MissingEntrypoint {
        path: PathBuf,
        error: String,
    },
    NoRules {
        path: PathBuf,
    },
    UnsupportedAbi {
        path: PathBuf,
        expected: u32,
        actual: u32,
    },
    InvalidId {
        path: PathBuf,
    },
    NoHooks {
        path: PathBuf,
        id: String,
    },
    #[cfg(target_arch = "wasm32")]
    UnsupportedTarget {
        path: PathBuf,
    },
}

impl NativeRuleLoadError {
    fn into_diagnostic(self) -> LintDiagnostic {
        let message = match self {
            Self::Open { path, error } => {
                format!("native lint rule library `{}` could not be loaded: {error}", path.display())
            }
            Self::MissingEntrypoint { path, error } => format!(
                "native lint rule library `{}` does not export `harn_native_lint_register_v1`: {error}",
                path.display()
            ),
            Self::NoRules { path } => format!(
                "native lint rule library `{}` did not register any rules",
                path.display()
            ),
            Self::UnsupportedAbi {
                path,
                expected,
                actual,
            } => format!(
                "native lint rule library `{}` uses ABI version {actual}, but this harn binary expects {expected}",
                path.display()
            ),
            Self::InvalidId { path } => format!(
                "native lint rule library `{}` registered a rule with an empty or invalid id",
                path.display()
            ),
            Self::NoHooks { path, id } => format!(
                "native lint rule library `{}` registered rule `{id}` without any hooks",
                path.display()
            ),
            #[cfg(target_arch = "wasm32")]
            Self::UnsupportedTarget { path } => format!(
                "native lint rule library `{}` cannot be loaded on this target",
                path.display()
            ),
        };
        LintDiagnostic {
            code: DiagnosticCode::LintRuleEngine,
            rule: LOADER_RULE_ID.into(),
            message,
            span: Span {
                start: 0,
                end: 0,
                line: 1,
                column: 1,
                end_line: 1,
            },
            severity: LintSeverity::Error,
            suggestion: None,
            fix: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::{
        HarnNativeDiagnostic, HarnNativeFixEdit, HarnNativeSpan, HARN_NATIVE_SEVERITY_ERROR,
    };

    #[test]
    fn maps_native_diagnostic_with_fix() {
        let span = HarnNativeSpan::new(10, 14, 2, 3);
        let fixes = [HarnNativeFixEdit::replace(span, "done")];
        let native = HarnNativeDiagnostic::warning("no marker", span)
            .with_suggestion("replace marker")
            .with_fixes(&fixes);

        let diag = diagnostic_from_native("native-no-marker", native).expect("diagnostic");
        assert_eq!(diag.rule.as_ref(), "native-no-marker");
        assert_eq!(diag.message, "no marker");
        assert_eq!(diag.severity, LintSeverity::Warning);
        assert_eq!((diag.span.start, diag.span.end), (10, 14));
        let fix = diag.fix.expect("fix");
        assert_eq!(fix[0].replacement, "done");
    }

    #[test]
    fn maps_error_severity_and_skips_empty_message() {
        let span = HarnNativeSpan::new(0, 0, 0, 0);
        let mut native = HarnNativeDiagnostic::warning("boom", span);
        native.severity = HARN_NATIVE_SEVERITY_ERROR;
        let diag = diagnostic_from_native("r", native).expect("diagnostic");
        assert_eq!(diag.severity, LintSeverity::Error);
        assert_eq!((diag.span.line, diag.span.column), (1, 1));

        native.message = HarnNativeStr::empty();
        assert!(diagnostic_from_native("r", native).is_none());
    }
}
