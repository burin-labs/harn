//! Repair vocabulary attached to a diagnostic code.
//!
//! A diagnostic says what is wrong; a repair says what an agent or editor may
//! do about it and how far that action is allowed to reach. The safety ladder
//! here is a contract surface — `harn fix --safety <class>` and IDE auto-apply
//! ceilings both compare against it — so the wire strings are as stable as the
//! codes themselves.
//!
//! Split out of `diagnostic_codes.rs` (#6126), which had reached its
//! source-length ceiling and could not accept another code. The registry keeps
//! the codes; this module keeps everything that answers "and what do I do".

use std::{fmt, str::FromStr};

use super::Code;

/// Autonomy ceiling of a proposed repair.
///
/// Agents and IDEs use this class to auto-apply, suggest, or escalate a fix. Variants
/// are ordered from least to most disruptive — call sites can compare
/// with `<=` to enforce a configured ceiling like
/// `"apply anything up to behavior-preserving"`.
///
/// The wire-format strings (`format-only`, `behavior-preserving`, …) are
/// the contract surface; renaming a variant string is a breaking change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RepairSafety {
    /// Whitespace, trivia, or canonical layout only. No code structure
    /// changes; safe to auto-apply.
    FormatOnly,
    /// Intended not to change observable runtime behavior (e.g. delete an
    /// unreachable branch, drop a redundant cast).
    BehaviorPreserving,
    /// Confined to the current local scope or file. Runtime behavior may
    /// change, but the blast radius does not cross a declaration boundary
    /// or a public surface.
    ScopeLocal,
    /// Touches a signature, export, or call-site surface that other files
    /// or external consumers can observe.
    SurfaceChanging,
    /// Required capabilities or sandbox profile may change as a result of
    /// applying the repair (e.g. swapping `provider: "openai"` for a
    /// capability flag widens the routing surface).
    CapabilityChanging,
    /// Planning hint only — agents should propose, never auto-apply.
    /// Aligned with the `AutonomyTier::Suggest`/`ActWithApproval` rungs
    /// in `trust_graph.rs`.
    NeedsHuman,
}

impl RepairSafety {
    pub const ALL: &'static [RepairSafety] = &[
        RepairSafety::FormatOnly,
        RepairSafety::BehaviorPreserving,
        RepairSafety::ScopeLocal,
        RepairSafety::SurfaceChanging,
        RepairSafety::CapabilityChanging,
        RepairSafety::NeedsHuman,
    ];

    /// Stable wire-format string. The contract surface — do not rename
    /// without coordinating with `harn fix --safety <…>` callers and
    /// downstream LSP/IDE clients.
    pub const fn as_str(self) -> &'static str {
        match self {
            RepairSafety::FormatOnly => "format-only",
            RepairSafety::BehaviorPreserving => "behavior-preserving",
            RepairSafety::ScopeLocal => "scope-local",
            RepairSafety::SurfaceChanging => "surface-changing",
            RepairSafety::CapabilityChanging => "capability-changing",
            RepairSafety::NeedsHuman => "needs-human",
        }
    }

    /// True when `self` sits at or below `ceiling`. Used by
    /// `harn fix --apply --safety <ceiling>` and IDE auto-apply policies
    /// to decide whether a repair clears the configured autonomy bar.
    pub const fn is_at_most(self, ceiling: RepairSafety) -> bool {
        (self as u8) <= (ceiling as u8)
    }

    /// Whether an editor or `harn lint --fix` may apply this repair without
    /// an explicit safety opt-in.
    pub const fn is_machine_applicable(self) -> bool {
        self.is_at_most(RepairSafety::BehaviorPreserving)
    }
}

impl fmt::Display for RepairSafety {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error returned when parsing an unknown repair-safety string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseRepairSafetyError;

impl fmt::Display for ParseRepairSafetyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("unknown Harn repair-safety class")
    }
}

impl std::error::Error for ParseRepairSafetyError {}

impl FromStr for RepairSafety {
    type Err = ParseRepairSafetyError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        RepairSafety::ALL
            .iter()
            .copied()
            .find(|safety| safety.as_str() == value)
            .ok_or(ParseRepairSafetyError)
    }
}

/// Namespaced kebab-case repair identifier (e.g. `imports/fix-path`).
///
/// Wraps a `Cow` so registry-driven values reuse a `'static` literal and
/// per-site overrides can still attach an owned string. The wire-format
/// string is the contract surface — never normalize or reformat on read.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RepairId(std::borrow::Cow<'static, str>);

impl RepairId {
    pub const fn from_static(s: &'static str) -> Self {
        RepairId(std::borrow::Cow::Borrowed(s))
    }

    pub fn from_owned(s: String) -> Self {
        RepairId(std::borrow::Cow::Owned(s))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RepairId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A structured repair proposal attached to a diagnostic.
///
/// `id` and `summary` are agent-readable metadata; `safety` is the
/// dispatch dimension that decides whether the repair clears an
/// autonomy ceiling. The concrete edits, when known statically, live on
/// the diagnostic's `fix: Option<Vec<FixEdit>>`; this `Repair` is the
/// classifier above those edits, not a replacement for them.
#[derive(Debug, Clone)]
pub struct Repair {
    pub id: RepairId,
    pub summary: String,
    pub safety: RepairSafety,
}

impl Repair {
    pub fn from_template(template: &RepairTemplate) -> Self {
        Repair {
            id: RepairId::from_static(template.id),
            summary: template.summary.to_string(),
            safety: template.safety,
        }
    }
}

/// Static-lifetime repair template bound to a diagnostic code.
///
/// Stored in the registry alongside `Code`. Construction sites can
/// materialize a `Repair` via [`Repair::from_template`] or override
/// `summary` for instance-specific detail by building a `Repair`
/// directly.
#[derive(Debug, Clone, Copy)]
pub struct RepairTemplate {
    pub id: &'static str,
    pub summary: &'static str,
    pub safety: RepairSafety,
}

impl Code {
    /// Look up the default repair template attached to this diagnostic
    /// code, or `None` if no actionable fix shape is registered.
    pub const fn repair_template(self) -> Option<&'static RepairTemplate> {
        match self {
            // --- TYP: type mismatches & coercions -------------------------
            Code::TypeMismatch
            | Code::ReturnTypeMismatch
            | Code::AssignmentTypeMismatch
            | Code::ArgumentTypeMismatch
            | Code::VariableTypeMismatch
            | Code::ClosureReturnTypeMismatch
            | Code::FieldTypeMismatch
            | Code::MethodTypeMismatch
            | Code::InvalidIndexType => Some(&REPAIR_INSERT_EXPLICIT_CONVERSION),
            Code::StringInterpolationRewrite => Some(&REPAIR_REWRITE_STRING_INTERPOLATION),
            Code::UnknownTypeName => Some(&REPAIR_IMPORTS_FIX_PATH),
            Code::ImplicitAnyParameter => Some(&REPAIR_TYPES_ANNOTATE_PARAMETER),
            Code::InvalidCast => Some(&REPAIR_CASTS_REMOVE_UNCHECKED),

            // --- NAM / IMP: imports & names -------------------------------
            Code::UndefinedVariable
            | Code::UndefinedFunction
            | Code::UnknownField
            | Code::UnknownMethod
            | Code::UnknownBuiltin
            | Code::UnknownDeclaration => Some(&REPAIR_BINDINGS_RENAME_TO_CLOSEST),
            Code::InvalidMainSignature => Some(&REPAIR_BINDINGS_THREAD_HARNESS_NEEDS_PARAM),
            Code::DeprecatedFunction => Some(&REPAIR_STDLIB_MIGRATE_RENAMED),
            Code::ModuleImportUnresolved | Code::ImportResolutionFailed => {
                Some(&REPAIR_IMPORTS_FIX_PATH)
            }
            Code::ModuleImportUnused => Some(&REPAIR_IMPORTS_REMOVE_UNUSED),
            Code::ModuleImportOrder => Some(&REPAIR_IMPORTS_REORDER),

            // --- CAP / RCV: capabilities & error recovery -----------------
            Code::CapabilityResultUnchecked => Some(&REPAIR_ERRORS_CHECK_OR_RESCUE),
            Code::CapabilityBindingInvalid => Some(&REPAIR_MANUAL_REVIEW_CAPABILITY),
            Code::EffectInheritanceViolation => Some(&REPAIR_POLICY_NARROW_CHILD_EFFECTS),
            Code::RescueOutsideFunction | Code::TryOutsideFunction => {
                Some(&REPAIR_ERRORS_WRAP_IN_FN)
            }

            // --- LLM / PRM: model + prompt contract -----------------------
            Code::LlmSchemaMissing => Some(&REPAIR_LLM_ADD_SCHEMA),
            Code::LlmProviderIdentityBranch | Code::PromptProviderIdentityBranch => {
                Some(&REPAIR_LLM_USE_CAPABILITY_FLAG)
            }
            Code::PromptInjectionRisk => Some(&REPAIR_PROMPTS_ESCAPE_INJECTION),
            Code::PromptToolSurfaceUnknown | Code::PromptToolSurfaceDeferredReference => {
                Some(&REPAIR_PROMPTS_ADD_TOOL_TO_SURFACE)
            }
            Code::PromptVariantExplosion => Some(&REPAIR_MANUAL_NEEDS_HUMAN),

            // --- STD: stdlib usage ----------------------------------------
            Code::DeprecatedStdlibSymbol => Some(&REPAIR_STDLIB_MIGRATE_RENAMED),
            Code::LintMissingStdlibMetadata => Some(&REPAIR_DOC_ADD_STDLIB_METADATA),

            // --- OWN: ownership & mutability ------------------------------
            Code::ImmutableAssignment => Some(&REPAIR_BINDINGS_MAKE_MUTABLE),
            Code::MutableNeverReassigned => Some(&REPAIR_BINDINGS_MAKE_IMMUTABLE),

            // --- MAT: match exhaustiveness --------------------------------
            Code::NonExhaustiveMatch => Some(&REPAIR_MATCH_ADD_MISSING_ARMS),
            Code::DuplicateMatchArm => Some(&REPAIR_MATCH_REMOVE_DUPLICATE_ARM),

            // --- ORC: orchestration ---------------------------------------
            Code::UnreachableCode => Some(&REPAIR_DEAD_CODE_REMOVE),

            // --- FMT: formatter -------------------------------------------
            Code::FormatterWouldReformat | Code::FormatterTrailingComma => {
                Some(&REPAIR_FORMAT_REFORMAT)
            }

            // --- LNT: lints with structured fixes -------------------------
            Code::LintUnusedVariable
            | Code::LintUnusedPatternBinding
            | Code::LintUnusedParameter => Some(&REPAIR_BINDINGS_RENAME_UNUSED),
            Code::LintUnusedPipelineInput => Some(&REPAIR_BINDINGS_REMOVE_UNUSED_PIPELINE_INPUT),
            Code::LintCapabilityParameterName => Some(&REPAIR_BINDINGS_NAME_CAPABILITY_PARAMETER),
            Code::LintUnusedImport => Some(&REPAIR_IMPORTS_REMOVE_UNUSED),
            Code::LintUnusedFunction | Code::LintUnusedType => {
                Some(&REPAIR_DECLARATIONS_REMOVE_UNUSED)
            }
            Code::LintMutableNeverReassigned => Some(&REPAIR_BINDINGS_MAKE_IMMUTABLE),
            Code::LintImportOrder => Some(&REPAIR_IMPORTS_REORDER),
            Code::LintBlankLineBetweenItems
            | Code::LintTrailingComma
            | Code::LintUnnecessaryParentheses
            | Code::LintRequireFileHeader => Some(&REPAIR_FORMAT_REFORMAT),
            Code::LintLegacyDocComment => Some(&REPAIR_DOC_COMMENT_MIGRATE),
            Code::LintEmptyBlock => Some(&REPAIR_BLOCK_REMOVE_EMPTY),
            Code::LintUnnecessaryElseReturn | Code::LintLetThenReturn => {
                Some(&REPAIR_CONTROL_FLOW_FLATTEN)
            }
            Code::LintNilCoalesceNoop
            | Code::LintNilCoalesceSelfFallback
            | Code::LintRedundantNilTernary
            | Code::LintUnnecessarySafeNavigation
            | Code::LintUnnecessaryNonNullAssert
            | Code::LintPreferOptionalShorthand
            | Code::LintComparisonToBool
            | Code::LintPointlessComparison
            | Code::LintConstantLogicalOperand => Some(&REPAIR_EXPRESSION_SIMPLIFY),
            Code::LintUnnecessaryCast => Some(&REPAIR_CASTS_REMOVE_REDUNDANT),
            Code::LintRedundantClone => Some(&REPAIR_CLONE_REMOVE_REDUNDANT),
            Code::LintEagerCollectionConversion => Some(&REPAIR_COLLECTION_PREFER_LAZY),
            Code::LintDeadCodeAfterReturn => Some(&REPAIR_DEAD_CODE_REMOVE),
            Code::LintRenamedStdlibSymbol => Some(&REPAIR_STDLIB_MIGRATE_RENAMED),
            Code::LintAmbientClockBuiltin => Some(&REPAIR_BINDINGS_THREAD_HARNESS_CLOCK),
            Code::LintAmbientFsBuiltin => Some(&REPAIR_BINDINGS_THREAD_HARNESS_FS),
            Code::LintAmbientEnvBuiltin => Some(&REPAIR_BINDINGS_THREAD_HARNESS_ENV),
            Code::LintAmbientRandomBuiltin => Some(&REPAIR_BINDINGS_THREAD_HARNESS_RANDOM),
            Code::LintAmbientNetBuiltin => Some(&REPAIR_BINDINGS_THREAD_HARNESS_NET),
            Code::LintAmbientStdioBuiltin => Some(&REPAIR_BINDINGS_THREAD_HARNESS),
            Code::LintAmbientHarnessMethod => Some(&REPAIR_BINDINGS_THREAD_HARNESS_METHOD),
            Code::LintBroadHarnessParameter => Some(&REPAIR_BINDINGS_ATTENUATE_HARNESS),
            Code::LintRemovedLlmOptions => Some(&REPAIR_LLM_MIGRATE_REMOVED_OPTION),
            Code::LintTemplateProviderIdentityBranch => Some(&REPAIR_LLM_USE_CAPABILITY_FLAG),
            Code::LintPromptInjectionRisk => Some(&REPAIR_PROMPTS_ESCAPE_INJECTION),
            Code::LintShadowVariable => Some(&REPAIR_BINDINGS_RENAME_SHADOW),
            Code::LintNamingConvention => Some(&REPAIR_STYLE_RENAME_TO_CONVENTION),
            Code::LintUnhandledApprovalResult => Some(&REPAIR_ERRORS_CHECK_OR_RESCUE),
            Code::LintMissingHarndoc => Some(&REPAIR_DOC_ADD_HARNDOC),
            Code::LintDuplicateMatchArm => Some(&REPAIR_MATCH_REMOVE_DUPLICATE_ARM),
            // HARN-LNT-029 is the lint face of the boundary-validation rule, so
            // its repair has to validate. HARN-LNT-060 below is a different
            // complaint — an inline options dict bypassing the typed option
            // constructors — where naming the shape is the whole fix and no
            // untrusted payload is involved.
            Code::LintUntypedDictAccess => Some(&REPAIR_TYPES_VALIDATE_BOUNDARY_VALUE),
            Code::LintUnnormalizedOptions => Some(&REPAIR_TYPES_ADD_SHAPE_ANNOTATION),
            Code::LintMcpToolAnnotations => Some(&REPAIR_MANUAL_NEEDS_HUMAN),
            Code::LintTemplateVariantExplosion | Code::LintLongRunningWithoutCleanup => {
                Some(&REPAIR_MANUAL_NEEDS_HUMAN)
            }

            // Everything else: no statically known repair shape. Agents
            // should treat these as "diagnose only" until a repair is
            // registered.
            _ => None,
        }
    }
}

// Repair-id catalog. Each `RepairTemplate` carries a kebab-case
// namespaced id (`<namespace>/<verb-noun>`), a one-line summary written
// in the imperative voice, and a `RepairSafety` class.
//
// Conventions:
//   - Namespaces stay short: `bindings/`, `imports/`, `errors/`, `casts/`,
//     `format/`, `llm/`, `prompts/`, `match/`, `stdlib/`, `lint/`,
//     `doc/`, `style/`, `types/`, `manual/`.
//   - Summary starts with a verb ("Replace…", "Remove…", "Insert…").
//   - Safety must be the most permissive class that is still always true
//     for every site this template attaches to. When unsure, pick the
//     stricter class — agents tighten too-loose policies later, never
//     too-tight ones.

const REPAIR_INSERT_EXPLICIT_CONVERSION: RepairTemplate = RepairTemplate {
    id: "casts/insert-explicit-conversion",
    summary: "Insert an explicit conversion or correct the operand type",
    safety: RepairSafety::ScopeLocal,
};

const REPAIR_REWRITE_STRING_INTERPOLATION: RepairTemplate = RepairTemplate {
    id: "style/string-interpolation",
    summary: "Rewrite string concatenation as an interpolation literal",
    safety: RepairSafety::BehaviorPreserving,
};

const REPAIR_CASTS_REMOVE_UNCHECKED: RepairTemplate = RepairTemplate {
    id: "casts/remove-unchecked",
    summary: "Remove the unchecked cast or guard it with a type test",
    safety: RepairSafety::ScopeLocal,
};

const REPAIR_CASTS_REMOVE_REDUNDANT: RepairTemplate = RepairTemplate {
    id: "casts/remove-redundant",
    summary: "Remove the redundant cast",
    safety: RepairSafety::BehaviorPreserving,
};

const REPAIR_BINDINGS_RENAME_TO_CLOSEST: RepairTemplate = RepairTemplate {
    id: "bindings/rename-to-closest",
    summary: "Rename to the closest in-scope identifier",
    safety: RepairSafety::ScopeLocal,
};

const REPAIR_BINDINGS_MAKE_MUTABLE: RepairTemplate = RepairTemplate {
    id: "bindings/make-mutable",
    summary: "Declare the binding with `let` so it can be reassigned",
    safety: RepairSafety::ScopeLocal,
};

const REPAIR_BINDINGS_MAKE_IMMUTABLE: RepairTemplate = RepairTemplate {
    id: "bindings/make-immutable",
    summary: "Declare the never-reassigned binding with `const` instead of `let`",
    safety: RepairSafety::BehaviorPreserving,
};

const REPAIR_BINDINGS_RENAME_UNUSED: RepairTemplate = RepairTemplate {
    id: "bindings/rename-unused",
    summary: "Mark an unused binding without changing callable arity",
    safety: RepairSafety::BehaviorPreserving,
};

const REPAIR_BINDINGS_REMOVE_UNUSED_PIPELINE_INPUT: RepairTemplate = RepairTemplate {
    id: "bindings/remove-unused-pipeline-input",
    summary: "Remove an explicitly unused test pipeline input",
    safety: RepairSafety::SurfaceChanging,
};

const REPAIR_BINDINGS_NAME_CAPABILITY_PARAMETER: RepairTemplate = RepairTemplate {
    id: "bindings/name-capability-parameter",
    summary: "Rename the capability parameter and its references after the capability it carries",
    safety: RepairSafety::SurfaceChanging,
};

const REPAIR_BINDINGS_RENAME_SHADOW: RepairTemplate = RepairTemplate {
    id: "bindings/rename-shadow",
    summary: "Rename the shadowing binding to a distinct name",
    safety: RepairSafety::ScopeLocal,
};

const REPAIR_BINDINGS_THREAD_HARNESS: RepairTemplate = RepairTemplate {
    id: "bindings/thread-harness",
    summary: "Thread the existing `harness` binding through local helper calls and replace the ambient stdio builtin with `harness.stdio.*`",
    safety: RepairSafety::ScopeLocal,
};

const REPAIR_BINDINGS_THREAD_HARNESS_NEEDS_PARAM: RepairTemplate = RepairTemplate {
    id: "bindings/thread-harness-needs-param",
    summary: "Add a `harness: Harness` parameter where the stdio capability handle is required and update local callers",
    safety: RepairSafety::SurfaceChanging,
};

const REPAIR_BINDINGS_THREAD_HARNESS_METHOD: RepairTemplate = RepairTemplate {
    id: "bindings/thread-harness-method",
    summary: "Replace the ambient runtime builtin with its typed `harness.*` method and thread authority through local callers",
    safety: RepairSafety::ScopeLocal,
};

const REPAIR_BINDINGS_THREAD_HARNESS_CLOCK: RepairTemplate = RepairTemplate {
    id: "bindings/thread-harness-clock",
    summary: "Replace the ambient clock builtin with the corresponding `harness.clock.*` method",
    safety: RepairSafety::ScopeLocal,
};

const REPAIR_BINDINGS_THREAD_HARNESS_FS: RepairTemplate = RepairTemplate {
    id: "bindings/thread-harness-fs",
    summary: "Replace the ambient fs builtin with the corresponding `harness.fs.*` method",
    safety: RepairSafety::ScopeLocal,
};

const REPAIR_BINDINGS_THREAD_HARNESS_ENV: RepairTemplate = RepairTemplate {
    id: "bindings/thread-harness-env",
    summary: "Replace the ambient env builtin with the corresponding `harness.env.*` method",
    safety: RepairSafety::ScopeLocal,
};

const REPAIR_BINDINGS_THREAD_HARNESS_RANDOM: RepairTemplate = RepairTemplate {
    id: "bindings/thread-harness-random",
    summary: "Replace the ambient random builtin with the corresponding `harness.random.*` method",
    safety: RepairSafety::ScopeLocal,
};

const REPAIR_BINDINGS_THREAD_HARNESS_NET: RepairTemplate = RepairTemplate {
    id: "bindings/thread-harness-net",
    summary: "Replace the ambient net builtin with the corresponding `harness.net.*` method",
    safety: RepairSafety::ScopeLocal,
};

const REPAIR_DECLARATIONS_REMOVE_UNUSED: RepairTemplate = RepairTemplate {
    id: "declarations/remove-unused",
    summary: "Remove the unused declaration",
    safety: RepairSafety::SurfaceChanging,
};

const REPAIR_IMPORTS_FIX_PATH: RepairTemplate = RepairTemplate {
    id: "imports/fix-path",
    summary: "Replace the import path with a resolvable target",
    safety: RepairSafety::ScopeLocal,
};

const REPAIR_IMPORTS_REMOVE_UNUSED: RepairTemplate = RepairTemplate {
    id: "imports/remove-unused",
    summary: "Remove the unused import",
    safety: RepairSafety::BehaviorPreserving,
};

const REPAIR_IMPORTS_REORDER: RepairTemplate = RepairTemplate {
    id: "imports/reorder",
    summary: "Reorder imports into canonical grouping",
    safety: RepairSafety::FormatOnly,
};

const REPAIR_ERRORS_CHECK_OR_RESCUE: RepairTemplate = RepairTemplate {
    id: "errors/check-or-rescue",
    summary: "Check the result or wrap the call in a `rescue` block",
    safety: RepairSafety::ScopeLocal,
};

const REPAIR_ERRORS_WRAP_IN_FN: RepairTemplate = RepairTemplate {
    id: "errors/wrap-in-fn",
    summary: "Move the construct inside a function body",
    safety: RepairSafety::SurfaceChanging,
};

const REPAIR_MATCH_ADD_MISSING_ARMS: RepairTemplate = RepairTemplate {
    id: "match/add-missing-arms",
    summary: "Add arms covering the missing variants",
    safety: RepairSafety::ScopeLocal,
};

const REPAIR_MATCH_REMOVE_DUPLICATE_ARM: RepairTemplate = RepairTemplate {
    id: "match/remove-duplicate-arm",
    summary: "Remove the duplicated match arm",
    safety: RepairSafety::BehaviorPreserving,
};

const REPAIR_FORMAT_REFORMAT: RepairTemplate = RepairTemplate {
    id: "format/reformat",
    summary: "Apply canonical formatting",
    safety: RepairSafety::FormatOnly,
};

const REPAIR_DOC_COMMENT_MIGRATE: RepairTemplate = RepairTemplate {
    id: "doc/migrate-comment-style",
    summary: "Migrate the legacy comment to canonical doc syntax",
    safety: RepairSafety::FormatOnly,
};

const REPAIR_DOC_ADD_HARNDOC: RepairTemplate = RepairTemplate {
    id: "doc/add-harndoc",
    summary: "Add a `///` doc comment describing this declaration",
    safety: RepairSafety::BehaviorPreserving,
};

const REPAIR_DOC_ADD_STDLIB_METADATA: RepairTemplate = RepairTemplate {
    id: "doc/add-stdlib-metadata",
    summary: "Add `@effects` and `@errors` fields to the stdlib function's doc block",
    safety: RepairSafety::BehaviorPreserving,
};

const REPAIR_BLOCK_REMOVE_EMPTY: RepairTemplate = RepairTemplate {
    id: "blocks/remove-empty",
    summary: "Remove the empty block or fill in an explicit body",
    safety: RepairSafety::ScopeLocal,
};

const REPAIR_CONTROL_FLOW_FLATTEN: RepairTemplate = RepairTemplate {
    id: "control-flow/flatten",
    summary: "Flatten the unnecessary control flow construct",
    safety: RepairSafety::BehaviorPreserving,
};

const REPAIR_EXPRESSION_SIMPLIFY: RepairTemplate = RepairTemplate {
    id: "expressions/simplify",
    summary: "Simplify the expression to its canonical form",
    safety: RepairSafety::BehaviorPreserving,
};

const REPAIR_CLONE_REMOVE_REDUNDANT: RepairTemplate = RepairTemplate {
    id: "clones/remove-redundant",
    summary: "Remove the redundant clone",
    safety: RepairSafety::BehaviorPreserving,
};

const REPAIR_COLLECTION_PREFER_LAZY: RepairTemplate = RepairTemplate {
    id: "collections/prefer-lazy",
    summary: "Replace the eager collection step with a lazy variant",
    safety: RepairSafety::ScopeLocal,
};

const REPAIR_DEAD_CODE_REMOVE: RepairTemplate = RepairTemplate {
    id: "control-flow/remove-dead",
    summary: "Remove the unreachable code",
    safety: RepairSafety::BehaviorPreserving,
};

const REPAIR_STDLIB_MIGRATE_RENAMED: RepairTemplate = RepairTemplate {
    id: "stdlib/migrate-renamed",
    summary: "Rename the call to the renamed stdlib symbol",
    safety: RepairSafety::ScopeLocal,
};

const REPAIR_BINDINGS_ATTENUATE_HARNESS: RepairTemplate = RepairTemplate {
    id: "bindings/attenuate-harness",
    summary: "Replace the root Harness parameter with the single capability the helper uses",
    safety: RepairSafety::SurfaceChanging,
};

const REPAIR_LLM_MIGRATE_REMOVED_OPTION: RepairTemplate = RepairTemplate {
    id: "llm/migrate-removed-option",
    summary: "Replace the removed option with its supported equivalent",
    safety: RepairSafety::ScopeLocal,
};

const REPAIR_LLM_ADD_SCHEMA: RepairTemplate = RepairTemplate {
    id: "llm/add-schema",
    summary: "Add a typed output schema to the LLM call",
    safety: RepairSafety::SurfaceChanging,
};

const REPAIR_LLM_USE_CAPABILITY_FLAG: RepairTemplate = RepairTemplate {
    id: "llm/use-capability-flag",
    summary: "Branch on a capability flag instead of provider identity",
    safety: RepairSafety::CapabilityChanging,
};

const REPAIR_PROMPTS_ESCAPE_INJECTION: RepairTemplate = RepairTemplate {
    id: "prompts/escape-injection",
    summary: "Pass the untrusted input through a structured placeholder",
    safety: RepairSafety::ScopeLocal,
};

const REPAIR_PROMPTS_ADD_TOOL_TO_SURFACE: RepairTemplate = RepairTemplate {
    id: "prompts/add-tool-to-surface",
    summary: "Add the referenced tool to the declared tool surface",
    safety: RepairSafety::SurfaceChanging,
};

const REPAIR_STYLE_RENAME_TO_CONVENTION: RepairTemplate = RepairTemplate {
    id: "style/rename-to-convention",
    summary: "Rename to match the casing convention for this kind",
    safety: RepairSafety::SurfaceChanging,
};

const REPAIR_TYPES_ADD_SHAPE_ANNOTATION: RepairTemplate = RepairTemplate {
    id: "types/add-shape-annotation",
    summary: "Annotate the dict with a concrete shape type",
    safety: RepairSafety::SurfaceChanging,
};

/// The repair for a parameter that never got a type.
///
/// Surface-changing rather than scope-local: the annotation lands on a
/// signature, and once it is there every caller is checked against it. That is
/// the point of the repair, and it is also why it is not auto-applied under a
/// lower ceiling. `harn fix` infers the type from the body and the call sites,
/// so the mechanical part of the migration is covered; `unknown` is what it
/// writes when it can prove nothing, preserving the need to narrow dynamic
/// values before use.
const REPAIR_TYPES_ANNOTATE_PARAMETER: RepairTemplate = RepairTemplate {
    id: "types/annotate-parameter",
    summary: "Annotate the parameter with the inferred type, or `unknown` and narrow it at the dynamic boundary",
    safety: RepairSafety::SurfaceChanging,
};

/// The repair for a boundary value read without validation.
///
/// Since harn#6252 a binding annotation *is* enforced, so annotating a
/// `json_parse` result does validate it — the original reason for keeping
/// `types/add-shape-annotation` away from this rule (that the annotation was
/// erased, and so removed the diagnostic without removing the hazard) no longer
/// holds.
///
/// This still offers schema validation rather than the annotation, for a
/// different and narrower reason: a binding assertion reports the declared type
/// and the value's kind, while `schema_expect` reports which field failed and
/// why. For an untrusted payload that difference is the whole diagnosis. The
/// annotation is now a correct fix; it is not the most informative one, and a
/// one-step automated repair should offer the most informative.
const REPAIR_TYPES_VALIDATE_BOUNDARY_VALUE: RepairTemplate = RepairTemplate {
    id: "types/validate-boundary-value",
    summary: "Validate the parsed value with schema_expect() or schema_check() before reading it",
    safety: RepairSafety::ScopeLocal,
};

const REPAIR_MANUAL_REVIEW_CAPABILITY: RepairTemplate = RepairTemplate {
    id: "manual/review-capability-binding",
    summary: "Review the capability binding; the fix is not mechanical",
    safety: RepairSafety::NeedsHuman,
};

const REPAIR_POLICY_NARROW_CHILD_EFFECTS: RepairTemplate = RepairTemplate {
    id: "policy/narrow-child-effects",
    summary: "Narrow the child agent's effects to a subset of the parent's, or widen the parent's declared effects",
    safety: RepairSafety::SurfaceChanging,
};

const REPAIR_MANUAL_NEEDS_HUMAN: RepairTemplate = RepairTemplate {
    id: "manual/needs-human",
    summary: "Plan a human-led change; auto-apply is not safe here",
    safety: RepairSafety::NeedsHuman,
};

/// Every repair template registered by [`Code::repair_template`], in source
/// order for catalog generation and health checks.
pub const REPAIR_REGISTRY: &[&RepairTemplate] = &[
    &REPAIR_INSERT_EXPLICIT_CONVERSION,
    &REPAIR_REWRITE_STRING_INTERPOLATION,
    &REPAIR_CASTS_REMOVE_UNCHECKED,
    &REPAIR_CASTS_REMOVE_REDUNDANT,
    &REPAIR_BINDINGS_RENAME_TO_CLOSEST,
    &REPAIR_BINDINGS_MAKE_MUTABLE,
    &REPAIR_BINDINGS_MAKE_IMMUTABLE,
    &REPAIR_BINDINGS_RENAME_UNUSED,
    &REPAIR_BINDINGS_REMOVE_UNUSED_PIPELINE_INPUT,
    &REPAIR_BINDINGS_NAME_CAPABILITY_PARAMETER,
    &REPAIR_BINDINGS_RENAME_SHADOW,
    &REPAIR_BINDINGS_THREAD_HARNESS,
    &REPAIR_BINDINGS_THREAD_HARNESS_NEEDS_PARAM,
    &REPAIR_BINDINGS_THREAD_HARNESS_METHOD,
    &REPAIR_BINDINGS_THREAD_HARNESS_CLOCK,
    &REPAIR_BINDINGS_THREAD_HARNESS_FS,
    &REPAIR_BINDINGS_THREAD_HARNESS_ENV,
    &REPAIR_BINDINGS_THREAD_HARNESS_RANDOM,
    &REPAIR_BINDINGS_THREAD_HARNESS_NET,
    &REPAIR_DECLARATIONS_REMOVE_UNUSED,
    &REPAIR_IMPORTS_FIX_PATH,
    &REPAIR_IMPORTS_REMOVE_UNUSED,
    &REPAIR_IMPORTS_REORDER,
    &REPAIR_ERRORS_CHECK_OR_RESCUE,
    &REPAIR_ERRORS_WRAP_IN_FN,
    &REPAIR_MATCH_ADD_MISSING_ARMS,
    &REPAIR_MATCH_REMOVE_DUPLICATE_ARM,
    &REPAIR_FORMAT_REFORMAT,
    &REPAIR_DOC_COMMENT_MIGRATE,
    &REPAIR_DOC_ADD_HARNDOC,
    &REPAIR_DOC_ADD_STDLIB_METADATA,
    &REPAIR_BLOCK_REMOVE_EMPTY,
    &REPAIR_CONTROL_FLOW_FLATTEN,
    &REPAIR_EXPRESSION_SIMPLIFY,
    &REPAIR_CLONE_REMOVE_REDUNDANT,
    &REPAIR_COLLECTION_PREFER_LAZY,
    &REPAIR_DEAD_CODE_REMOVE,
    &REPAIR_STDLIB_MIGRATE_RENAMED,
    &REPAIR_BINDINGS_ATTENUATE_HARNESS,
    &REPAIR_LLM_MIGRATE_REMOVED_OPTION,
    &REPAIR_LLM_ADD_SCHEMA,
    &REPAIR_LLM_USE_CAPABILITY_FLAG,
    &REPAIR_PROMPTS_ESCAPE_INJECTION,
    &REPAIR_PROMPTS_ADD_TOOL_TO_SURFACE,
    &REPAIR_STYLE_RENAME_TO_CONVENTION,
    &REPAIR_TYPES_ADD_SHAPE_ANNOTATION,
    &REPAIR_TYPES_ANNOTATE_PARAMETER,
    &REPAIR_TYPES_VALIDATE_BOUNDARY_VALUE,
    &REPAIR_MANUAL_REVIEW_CAPABILITY,
    &REPAIR_MANUAL_NEEDS_HUMAN,
    &REPAIR_POLICY_NARROW_CHILD_EFFECTS,
];

#[cfg(test)]
mod tests {
    use super::super::Category;
    use super::{Code, ParseRepairSafetyError, RepairSafety, REPAIR_REGISTRY};
    use std::collections::HashSet;
    use std::str::FromStr;

    #[test]
    fn parses_registered_code() {
        assert_eq!(Code::from_str("HARN-TYP-014"), Ok(Code::TypeParameterArity));
    }

    #[test]
    fn registry_has_unique_identifiers() {
        let mut seen = HashSet::new();
        for entry in Code::registry() {
            assert!(
                seen.insert(entry.identifier),
                "duplicate diagnostic code {}",
                entry.identifier
            );
            assert_eq!(entry.code.as_str(), entry.identifier);
            assert_eq!(entry.code.category(), entry.category);
            let expected_prefix = format!("HARN-{}-", entry.category);
            assert!(entry.identifier.starts_with(&expected_prefix));
            let suffix = entry.identifier.trim_start_matches(&expected_prefix);
            assert_eq!(suffix.len(), 3);
            assert!(suffix.chars().all(|ch| ch.is_ascii_digit()));
            assert!(!entry.summary.is_empty());
        }
        assert!(Code::registry().len() >= 40);
    }

    #[test]
    fn every_category_is_populated() {
        for category in Category::ALL {
            assert!(
                Code::registry()
                    .iter()
                    .any(|entry| entry.category == *category),
                "missing diagnostic code category {category}"
            );
        }
    }

    #[test]
    fn every_code_has_non_empty_explanation() {
        for entry in Code::registry() {
            let body = entry.code.explanation();
            assert!(
                !body.trim().is_empty(),
                "diagnostic code {} has an empty explanation file",
                entry.identifier
            );
            assert!(
                body.contains(entry.identifier),
                "explanation for {} should reference its identifier",
                entry.identifier
            );
        }
    }

    #[test]
    fn related_codes_are_registered_and_non_self() {
        for entry in Code::registry() {
            for &other in entry.code.related() {
                assert_ne!(
                    other, entry.code,
                    "{} lists itself as a related code",
                    entry.identifier
                );
                assert!(
                    Code::registry().iter().any(|e| e.code == other),
                    "{} lists unregistered related code {}",
                    entry.identifier,
                    other
                );
            }
        }
    }

    #[test]
    fn repair_safety_string_roundtrip() {
        for safety in RepairSafety::ALL {
            let parsed = RepairSafety::from_str(safety.as_str()).unwrap();
            assert_eq!(parsed, *safety);
            assert_eq!(parsed.to_string(), safety.as_str());
        }
        assert_eq!(
            RepairSafety::from_str("not-a-safety-class"),
            Err(ParseRepairSafetyError)
        );
    }

    #[test]
    fn repair_safety_ordering_is_monotonic_low_to_high() {
        // The is_at_most ceiling check relies on this ordering being
        // least-to-most disruptive; a regression here flips the meaning
        // of `harn fix --safety <ceiling>` for every caller.
        let order = RepairSafety::ALL;
        for window in order.windows(2) {
            assert!(
                window[0] < window[1],
                "{:?} should be safer than {:?}",
                window[0],
                window[1]
            );
            assert!(window[0].is_at_most(window[1]));
            assert!(!window[1].is_at_most(window[0]));
        }
    }

    #[test]
    fn only_behavior_preserving_repairs_are_machine_applicable() {
        assert!(RepairSafety::FormatOnly.is_machine_applicable());
        assert!(RepairSafety::BehaviorPreserving.is_machine_applicable());
        for safety in [
            RepairSafety::ScopeLocal,
            RepairSafety::SurfaceChanging,
            RepairSafety::CapabilityChanging,
            RepairSafety::NeedsHuman,
        ] {
            assert!(!safety.is_machine_applicable(), "{safety}");
        }
    }

    #[test]
    fn repair_registry_has_at_least_twenty_entries() {
        assert!(
            REPAIR_REGISTRY.len() >= 20,
            "expected ≥20 repair templates, found {}",
            REPAIR_REGISTRY.len()
        );
    }

    #[test]
    fn repair_ids_are_kebab_case_namespaced_and_unique() {
        let mut seen = HashSet::new();
        for template in REPAIR_REGISTRY {
            assert!(
                seen.insert(template.id),
                "duplicate repair id {}",
                template.id
            );
            let (namespace, leaf) = template.id.split_once('/').unwrap_or_else(|| {
                panic!(
                    "repair id `{}` is missing `<namespace>/` prefix",
                    template.id
                )
            });
            assert!(
                !namespace.is_empty() && !leaf.is_empty(),
                "repair id `{}` has empty namespace or leaf",
                template.id
            );
            for ch in template.id.chars() {
                assert!(
                    ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-' || ch == '/',
                    "repair id `{}` has non-kebab character {ch:?}",
                    template.id
                );
            }
            assert!(
                !template.summary.is_empty(),
                "repair {} has empty summary",
                template.id
            );
            // Summaries are imperative: start with a capital ASCII letter.
            let first = template.summary.chars().next().unwrap();
            assert!(
                first.is_ascii_uppercase(),
                "repair {} summary `{}` should start with a capital",
                template.id,
                template.summary
            );
        }
    }

    #[test]
    fn manual_namespace_is_needs_human() {
        for template in REPAIR_REGISTRY {
            if let Some(("manual", _)) = template.id.split_once('/') {
                assert_eq!(
                    template.safety,
                    RepairSafety::NeedsHuman,
                    "manual/* repair {} must be NeedsHuman",
                    template.id
                );
            }
        }
    }

    #[test]
    fn known_codes_carry_expected_safety_class() {
        // Spot-check: the autonomy contract for several representative
        // diagnostics. Lock in the safety class so cross-repo agents that
        // dispatch on these don't silently drift when the catalog moves.
        let expected: &[(Code, RepairSafety, &str)] = &[
            (
                Code::FormatterWouldReformat,
                RepairSafety::FormatOnly,
                "format/reformat",
            ),
            (
                Code::ModuleImportUnused,
                RepairSafety::BehaviorPreserving,
                "imports/remove-unused",
            ),
            (
                Code::ImmutableAssignment,
                RepairSafety::ScopeLocal,
                "bindings/make-mutable",
            ),
            (
                Code::LintUnusedFunction,
                RepairSafety::SurfaceChanging,
                "declarations/remove-unused",
            ),
            (
                Code::LlmProviderIdentityBranch,
                RepairSafety::CapabilityChanging,
                "llm/use-capability-flag",
            ),
            (
                Code::PromptVariantExplosion,
                RepairSafety::NeedsHuman,
                "manual/needs-human",
            ),
            (
                Code::NonExhaustiveMatch,
                RepairSafety::ScopeLocal,
                "match/add-missing-arms",
            ),
            (
                Code::LintAmbientClockBuiltin,
                RepairSafety::ScopeLocal,
                "bindings/thread-harness-clock",
            ),
            (
                Code::LintAmbientStdioBuiltin,
                RepairSafety::ScopeLocal,
                "bindings/thread-harness",
            ),
            (
                Code::InvalidMainSignature,
                RepairSafety::SurfaceChanging,
                "bindings/thread-harness-needs-param",
            ),
        ];
        for (code, safety, repair_id) in expected {
            let template = code
                .repair_template()
                .unwrap_or_else(|| panic!("{code} should have a repair template"));
            assert_eq!(template.safety, *safety, "{code} safety class drifted");
            assert_eq!(template.id, *repair_id, "{code} repair id drifted");
        }
    }

    #[test]
    fn repair_templates_cover_at_least_twenty_codes() {
        let covered = Code::ALL
            .iter()
            .filter(|code| code.repair_template().is_some())
            .count();
        assert!(
            covered >= 20,
            "expected ≥20 codes with a repair template, found {covered}"
        );
    }

    /// A rule about validating a boundary value cannot be repaired by an
    /// annotation.
    ///
    /// An annotation is erased before the value exists: annotating a
    /// `json_parse` result reads an int out of a field declared `string` and
    /// accepts an array as a record, with no diagnostic at either compile time
    /// or run time. Offering it as the one-step repair for these codes would
    /// auto-apply the escape the codes exist to close, and a repair is applied
    /// more readily than help text is read (harn#6234).
    ///
    /// `LintUnnormalizedOptions` is deliberately not in this list. It is about
    /// an inline options dict bypassing the typed option constructors, where no
    /// untrusted payload is involved and naming the shape is the whole fix.
    #[test]
    fn boundary_validation_codes_offer_the_most_informative_repair() {
        for code in [Code::BoundaryValueUnvalidated, Code::LintUntypedDictAccess] {
            let Some(template) = code.repair_template() else {
                continue;
            };
            assert_ne!(
                template.id, "types/add-shape-annotation",
                "{code:?} is a boundary-validation rule; an annotation validates the value \
                 (harn#6252) but reports only the declared type and the value's kind, so the \
                 offered one-step repair must be the one that names the failing field"
            );
        }
    }

    #[test]
    fn every_registered_repair_is_referenced_by_some_code() {
        let referenced: HashSet<&'static str> = Code::ALL
            .iter()
            .filter_map(|code| code.repair_template())
            .map(|template| template.id)
            .collect();
        for template in REPAIR_REGISTRY {
            assert!(
                referenced.contains(template.id),
                "repair {} is in REPAIR_REGISTRY but no Code maps to it",
                template.id
            );
        }
    }

    #[test]
    fn every_referenced_repair_template_is_in_registry() {
        let registered: HashSet<&'static str> =
            REPAIR_REGISTRY.iter().map(|template| template.id).collect();
        for code in Code::ALL {
            let Some(template) = code.repair_template() else {
                continue;
            };
            assert!(
                registered.contains(template.id),
                "repair {} (used by {}) is missing from REPAIR_REGISTRY",
                template.id,
                code
            );
        }
    }
}
