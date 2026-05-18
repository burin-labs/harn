//! Stable diagnostic code registry.
//!
//! Codes use `HARN-<CATEGORY>-<NNN>` identifiers so CLI output, editor
//! diagnostics, docs, and future `harn explain` lookups can refer to one
//! durable namespace.
//!
//! ```
//! use harn_parser::diagnostic_codes::Category;
//!
//! let categories: Vec<_> = Category::ALL.iter().map(|category| category.as_str()).collect();
//! assert_eq!(
//!     categories,
//!     [
//!         "TYP", "PAR", "NAM", "CAP", "LLM", "ORC", "STD", "PRM",
//!         "MOD", "LNT", "FMT", "IMP", "OWN", "RCV", "MAT",
//!     ],
//! );
//! ```

use std::fmt;
use std::str::FromStr;

/// Top-level diagnostic category used in a stable Harn diagnostic code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Category {
    Typ,
    Par,
    Nam,
    Cap,
    Llm,
    Orc,
    Std,
    Prm,
    Mod,
    Lnt,
    Fmt,
    Imp,
    Own,
    Rcv,
    Mat,
}

impl Category {
    pub const ALL: &'static [Category] = &[
        Category::Typ,
        Category::Par,
        Category::Nam,
        Category::Cap,
        Category::Llm,
        Category::Orc,
        Category::Std,
        Category::Prm,
        Category::Mod,
        Category::Lnt,
        Category::Fmt,
        Category::Imp,
        Category::Own,
        Category::Rcv,
        Category::Mat,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Category::Typ => "TYP",
            Category::Par => "PAR",
            Category::Nam => "NAM",
            Category::Cap => "CAP",
            Category::Llm => "LLM",
            Category::Orc => "ORC",
            Category::Std => "STD",
            Category::Prm => "PRM",
            Category::Mod => "MOD",
            Category::Lnt => "LNT",
            Category::Fmt => "FMT",
            Category::Imp => "IMP",
            Category::Own => "OWN",
            Category::Rcv => "RCV",
            Category::Mat => "MAT",
        }
    }
}

impl fmt::Display for Category {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One registered diagnostic code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegistryEntry {
    pub code: Code,
    pub identifier: &'static str,
    pub category: Category,
    pub summary: &'static str,
}

macro_rules! diagnostic_codes {
    ($($variant:ident, $identifier:literal, $category:ident, $summary:literal;)*) => {
        /// Stable diagnostic identifier.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum Code {
            $($variant,)*
        }

        impl Code {
            pub const ALL: &'static [Code] = &[
                $(Code::$variant,)*
            ];

            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Code::$variant => $identifier,)*
                }
            }

            pub const fn category(self) -> Category {
                match self {
                    $(Code::$variant => Category::$category,)*
                }
            }

            pub const fn summary(self) -> &'static str {
                match self {
                    $(Code::$variant => $summary,)*
                }
            }

            /// Full markdown explanation embedded at compile time. Every
            /// registered code must ship a matching file under
            /// `diagnostic_codes/explanations/`; missing files fail the build.
            pub const fn explanation(self) -> &'static str {
                match self {
                    $(Code::$variant => include_str!(
                        concat!("diagnostic_codes/explanations/", $identifier, ".md")
                    ),)*
                }
            }
        }

        pub const REGISTRY: &[RegistryEntry] = &[
            $(RegistryEntry {
                code: Code::$variant,
                identifier: $identifier,
                category: Category::$category,
                summary: $summary,
            },)*
        ];
    };
}

diagnostic_codes! {
    TypeMismatch, "HARN-TYP-001", Typ, "expected and actual types are incompatible";
    InvalidBinaryOperator, "HARN-TYP-002", Typ, "binary operator is not defined for the operand types";
    StringInterpolationRewrite, "HARN-TYP-003", Typ, "string concatenation should be rewritten as interpolation";
    ReturnTypeMismatch, "HARN-TYP-004", Typ, "returned expression does not match the declared return type";
    AssignmentTypeMismatch, "HARN-TYP-005", Typ, "assigned value does not match the target type";
    ArgumentTypeMismatch, "HARN-TYP-006", Typ, "argument value does not match the parameter type";
    VariableTypeMismatch, "HARN-TYP-007", Typ, "initializer does not match the declared variable type";
    ClosureReturnTypeMismatch, "HARN-TYP-008", Typ, "closure return expression does not match its declared type";
    FieldTypeMismatch, "HARN-TYP-009", Typ, "field value does not match its declared type";
    MethodTypeMismatch, "HARN-TYP-010", Typ, "method receiver or result type is incompatible";
    GenericTypeArgumentUnsupported, "HARN-TYP-011", Typ, "callable does not accept type arguments";
    GenericTypeArgumentMismatch, "HARN-TYP-012", Typ, "type argument does not satisfy the generic parameter";
    GenericTypeArgumentArity, "HARN-TYP-013", Typ, "generic call has the wrong number of type arguments";
    TypeParameterArity, "HARN-TYP-014", Typ, "declaration has the wrong number of type parameters";
    WhereConstraintMismatch, "HARN-TYP-015", Typ, "type argument does not satisfy a where-clause constraint";
    IterableExpected, "HARN-TYP-016", Typ, "expression must be iterable";
    InvalidIndexType, "HARN-TYP-017", Typ, "subscript index type is invalid";
    CallableExpected, "HARN-TYP-018", Typ, "expression must be callable";
    InvalidCast, "HARN-TYP-019", Typ, "cast cannot be proven valid";
    UnknownTypeName, "HARN-TYP-020", Typ, "type name cannot be resolved";
    InvalidVariantUse, "HARN-TYP-021", Typ, "variant type is used in an invalid position";
    InvalidStructLiteral, "HARN-TYP-022", Typ, "struct literal is invalid";
    InvalidEnumConstruct, "HARN-TYP-023", Typ, "enum construction is invalid";
    InvalidPatternBinding, "HARN-TYP-024", Typ, "pattern binding is invalid for the expected type";
    InvalidOptionalAccess, "HARN-TYP-025", Typ, "optional access is invalid for the receiver type";
    ParserUnexpectedToken, "HARN-PAR-001", Par, "parser found an unexpected token";
    ParserUnexpectedEof, "HARN-PAR-002", Par, "parser reached end of file while expecting syntax";
    ParserUnexpectedCharacter, "HARN-PAR-003", Par, "lexer found an unexpected character";
    ParserUnterminatedString, "HARN-PAR-004", Par, "string literal is unterminated";
    ParserUnterminatedBlockComment, "HARN-PAR-005", Par, "block comment is unterminated";
    UndefinedVariable, "HARN-NAM-001", Nam, "variable name cannot be resolved";
    UndefinedFunction, "HARN-NAM-002", Nam, "function name cannot be resolved";
    UnknownAttribute, "HARN-NAM-003", Nam, "attribute name is not recognized";
    UnknownField, "HARN-NAM-004", Nam, "field name does not exist on the target type";
    UnknownMethod, "HARN-NAM-005", Nam, "method name does not exist on the receiver type";
    DuplicateArgument, "HARN-NAM-006", Nam, "argument name is duplicated";
    UnknownOption, "HARN-NAM-007", Nam, "option key is not recognized";
    UnknownBuiltin, "HARN-NAM-008", Nam, "builtin name cannot be resolved";
    DeprecatedFunction, "HARN-NAM-009", Nam, "function call targets a deprecated declaration";
    UnknownDeclaration, "HARN-NAM-010", Nam, "declaration reference cannot be resolved";
    InvalidAttributeTarget, "HARN-NAM-011", Nam, "attribute is attached to an unsupported declaration";
    InvalidAttributeArgument, "HARN-NAM-012", Nam, "attribute argument is invalid";
    InvalidMainSignature, "HARN-NAM-101", Nam, "`main` entrypoint must take a single `harness: Harness` parameter";
    CapabilityPayloadInvalid, "HARN-CAP-001", Cap, "capability payload is invalid";
    HitlMissingApprovalPolicy, "HARN-CAP-002", Cap, "human approval construct is missing policy";
    HitlInvalidApprovalArgument, "HARN-CAP-003", Cap, "human approval argument is invalid";
    CapabilityResultUnchecked, "HARN-CAP-004", Cap, "capability result must be checked";
    CapabilityUnknownOperation, "HARN-CAP-005", Cap, "host capability operation is not declared";
    CapabilityCallStaticNameRequired, "HARN-CAP-006", Cap, "host capability call must use a static operation name";
    CapabilityBindingInvalid, "HARN-CAP-007", Cap, "tool host capability binding is invalid";
    UnknownLlmOption, "HARN-LLM-001", Llm, "LLM option key is not recognized";
    DeprecatedLlmOption, "HARN-LLM-002", Llm, "LLM option key is deprecated";
    LlmSchemaMissing, "HARN-LLM-003", Llm, "LLM call is missing schema validation";
    LlmSchemaInvalid, "HARN-LLM-004", Llm, "LLM schema option is invalid";
    LlmProviderIdentityBranch, "HARN-LLM-005", Llm, "prompt branches on provider identity instead of capability flags";
    OrchestrationArity, "HARN-ORC-001", Orc, "orchestration construct has invalid arity";
    OrchestrationType, "HARN-ORC-002", Orc, "orchestration construct argument has invalid type";
    AgentDefinitionInvalid, "HARN-ORC-003", Orc, "agent declaration is invalid";
    WorkflowDefinitionInvalid, "HARN-ORC-004", Orc, "workflow declaration is invalid";
    ToolDefinitionInvalid, "HARN-ORC-005", Orc, "tool declaration is invalid";
    PipelineDefinitionInvalid, "HARN-ORC-006", Orc, "pipeline declaration is invalid";
    InvalidSelectConstruct, "HARN-ORC-007", Orc, "select construct is invalid";
    UnreachableCode, "HARN-ORC-008", Orc, "statement cannot be reached";
    FlowInvariantAttributeInvalid, "HARN-ORC-009", Orc, "Flow invariant attribute set is invalid";
    ExecutionTargetMissing, "HARN-ORC-010", Orc, "execution target path cannot be found";
    DeprecatedStdlibSymbol, "HARN-STD-001", Std, "stdlib symbol has been renamed or deprecated";
    StdlibUsageInvalid, "HARN-STD-002", Std, "stdlib call is invalid";
    BuiltinArity, "HARN-STD-003", Std, "builtin call has invalid arity";
    PromptTemplateParse, "HARN-PRM-001", Prm, "prompt template cannot be parsed";
    PromptVariantExplosion, "HARN-PRM-002", Prm, "prompt template has too many capability-aware branches";
    PromptInjectionRisk, "HARN-PRM-003", Prm, "prompt construction risks direct injection";
    PromptProviderIdentityBranch, "HARN-PRM-004", Prm, "prompt template branches on provider identity";
    PromptToolSurfaceUnknown, "HARN-PRM-005", Prm, "prompt references a tool outside the declared surface";
    PromptToolSurfaceDeferredReference, "HARN-PRM-006", Prm, "prompt references a deferred tool without tool search";
    PromptTargetMissing, "HARN-PRM-007", Prm, "prompt or template target cannot be found";
    ModuleImportUnresolved, "HARN-MOD-001", Mod, "module import cannot be resolved";
    ModuleImportUnused, "HARN-MOD-002", Mod, "module import is unused";
    ModuleImportOrder, "HARN-MOD-003", Mod, "module imports are not in canonical order";
    ModuleExportInvalid, "HARN-MOD-004", Mod, "module export is invalid";
    ModuleImportCollision, "HARN-MOD-005", Mod, "module imports expose colliding names";
    ModuleReExportConflict, "HARN-MOD-006", Mod, "module re-exports conflict";
    LintRenamedStdlibSymbol, "HARN-LNT-001", Lnt, "renamed stdlib symbol lint";
    LintCyclomaticComplexity, "HARN-LNT-002", Lnt, "cyclomatic complexity lint";
    LintNamingConvention, "HARN-LNT-003", Lnt, "naming convention lint";
    LintEagerCollectionConversion, "HARN-LNT-004", Lnt, "eager collection conversion lint";
    LintRedundantClone, "HARN-LNT-005", Lnt, "redundant clone lint";
    LintLongRunningWithoutCleanup, "HARN-LNT-006", Lnt, "long-running workflow cleanup lint";
    LintMcpToolAnnotations, "HARN-LNT-007", Lnt, "MCP tool annotations lint";
    LintPrOpenWithoutSecretScan, "HARN-LNT-008", Lnt, "PR open without secret scan lint";
    LintShadowVariable, "HARN-LNT-009", Lnt, "shadow variable lint";
    LintPersonaHookTarget, "HARN-LNT-010", Lnt, "persona hook target lint";
    LintDeadCodeAfterReturn, "HARN-LNT-011", Lnt, "dead code after return lint";
    LintLetThenReturn, "HARN-LNT-012", Lnt, "let then return lint";
    LintUnhandledApprovalResult, "HARN-LNT-013", Lnt, "unhandled approval result lint";
    LintUnusedVariable, "HARN-LNT-014", Lnt, "unused variable lint";
    LintUnusedPatternBinding, "HARN-LNT-015", Lnt, "unused pattern binding lint";
    LintUnusedParameter, "HARN-LNT-016", Lnt, "unused parameter lint";
    LintUnusedImport, "HARN-LNT-017", Lnt, "unused import lint";
    LintMutableNeverReassigned, "HARN-LNT-018", Lnt, "mutable never reassigned lint";
    LintUnusedFunction, "HARN-LNT-019", Lnt, "unused function lint";
    LintUnusedType, "HARN-LNT-020", Lnt, "unused type lint";
    LintPersonaBodyMustCallSteps, "HARN-LNT-021", Lnt, "persona body must call steps lint";
    LintUndefinedFunction, "HARN-LNT-022", Lnt, "undefined function lint";
    LintPipelineReturnType, "HARN-LNT-023", Lnt, "pipeline return type lint";
    LintMissingHarndoc, "HARN-LNT-024", Lnt, "missing harndoc lint";
    LintAssertOutsideTest, "HARN-LNT-025", Lnt, "assert outside test lint";
    LintPromptInjectionRisk, "HARN-LNT-026", Lnt, "prompt injection risk lint";
    LintConnectorEffectPolicy, "HARN-LNT-027", Lnt, "connector effect policy lint";
    LintUnnecessaryCast, "HARN-LNT-028", Lnt, "unnecessary cast lint";
    LintUntypedDictAccess, "HARN-LNT-029", Lnt, "untyped dict access lint";
    LintConstantLogicalOperand, "HARN-LNT-030", Lnt, "constant logical operand lint";
    LintPointlessComparison, "HARN-LNT-031", Lnt, "pointless comparison lint";
    LintComparisonToBool, "HARN-LNT-032", Lnt, "comparison to bool lint";
    LintInvalidBinaryOpLiteral, "HARN-LNT-033", Lnt, "invalid binary operator literal lint";
    LintRedundantNilTernary, "HARN-LNT-034", Lnt, "redundant nil ternary lint";
    LintEmptyBlock, "HARN-LNT-035", Lnt, "empty block lint";
    LintUnnecessaryElseReturn, "HARN-LNT-036", Lnt, "unnecessary else return lint";
    LintDuplicateMatchArm, "HARN-LNT-037", Lnt, "duplicate match arm lint";
    LintRequireInTest, "HARN-LNT-038", Lnt, "require in test lint";
    LintBreakOutsideLoop, "HARN-LNT-039", Lnt, "break outside loop lint";
    LintTemplateParse, "HARN-LNT-040", Lnt, "template parse lint";
    LintBlankLineBetweenItems, "HARN-LNT-041", Lnt, "blank line between items lint";
    LintTrailingComma, "HARN-LNT-042", Lnt, "trailing comma lint";
    LintUnnecessaryParentheses, "HARN-LNT-043", Lnt, "unnecessary parentheses lint";
    LintTemplateVariantExplosion, "HARN-LNT-044", Lnt, "template variant explosion lint";
    LintRequireFileHeader, "HARN-LNT-045", Lnt, "require file header lint";
    LintTemplateProviderIdentityBranch, "HARN-LNT-046", Lnt, "template provider identity branch lint";
    LintImportOrder, "HARN-LNT-047", Lnt, "import order lint";
    LintPreferOptionalShorthand, "HARN-LNT-048", Lnt, "prefer optional shorthand lint";
    LintLegacyDocComment, "HARN-LNT-049", Lnt, "legacy doc comment lint";
    LintDeprecatedLlmOptions, "HARN-LNT-050", Lnt, "deprecated LLM options lint";
    LintUnnecessarySafeNavigation, "HARN-LNT-051", Lnt, "unnecessary safe navigation lint";
    LintAmbientClockBuiltin, "HARN-LNT-052", Lnt, "ambient clock builtin replaced by `harness.clock.*`";
    FormatterParseFailed, "HARN-FMT-001", Fmt, "formatter could not parse the source";
    FormatterWouldReformat, "HARN-FMT-002", Fmt, "source is not in canonical format";
    FormatterTrailingComma, "HARN-FMT-003", Fmt, "formatter normalized trailing comma layout";
    ImportResolutionFailed, "HARN-IMP-001", Imp, "import target cannot be resolved";
    ImportSymbolMissing, "HARN-IMP-002", Imp, "imported symbol does not exist";
    ImportCycle, "HARN-IMP-003", Imp, "import graph contains a cycle";
    ImmutableAssignment, "HARN-OWN-001", Own, "immutable binding is reassigned";
    MutableNeverReassigned, "HARN-OWN-002", Own, "mutable binding is never reassigned";
    OwnershipEscape, "HARN-OWN-003", Own, "owned value escapes its valid scope";
    BoundaryValueUnvalidated, "HARN-OWN-004", Own, "unvalidated boundary value is used directly";
    RescueOutsideFunction, "HARN-RCV-001", Rcv, "rescue construct is outside a function body";
    TryOutsideFunction, "HARN-RCV-002", Rcv, "try construct is outside a function body";
    InvalidRescueConstruct, "HARN-RCV-003", Rcv, "rescue construct is invalid";
    NonExhaustiveMatch, "HARN-MAT-001", Mat, "match expression is not exhaustive";
    DuplicateMatchArm, "HARN-MAT-002", Mat, "match expression contains a duplicate arm";
    InvalidMatchPattern, "HARN-MAT-003", Mat, "match pattern is invalid";
}

impl Code {
    pub const fn registry() -> &'static [RegistryEntry] {
        REGISTRY
    }

    /// Codes that an agent should consider alongside this one when planning
    /// repairs. Curated per-code — typically near-neighbours in the same
    /// category that share a fix shape. Returns an empty slice for codes
    /// without curated cross-references.
    pub const fn related(self) -> &'static [Code] {
        match self {
            // Type mismatches form a family — surfacing the others helps an
            // agent disambiguate between assignment, argument, return, etc.
            Code::TypeMismatch => &[
                Code::AssignmentTypeMismatch,
                Code::ArgumentTypeMismatch,
                Code::ReturnTypeMismatch,
                Code::VariableTypeMismatch,
                Code::FieldTypeMismatch,
            ],
            Code::AssignmentTypeMismatch => &[Code::TypeMismatch, Code::VariableTypeMismatch],
            Code::ArgumentTypeMismatch => &[Code::TypeMismatch, Code::GenericTypeArgumentMismatch],
            Code::ReturnTypeMismatch => &[Code::TypeMismatch, Code::ClosureReturnTypeMismatch],
            Code::VariableTypeMismatch => &[Code::TypeMismatch, Code::AssignmentTypeMismatch],
            Code::ClosureReturnTypeMismatch => &[Code::ReturnTypeMismatch],
            Code::FieldTypeMismatch => &[Code::TypeMismatch, Code::InvalidStructLiteral],
            Code::MethodTypeMismatch => &[Code::TypeMismatch, Code::CallableExpected],
            // Generic type-argument family.
            Code::GenericTypeArgumentUnsupported => &[
                Code::GenericTypeArgumentMismatch,
                Code::GenericTypeArgumentArity,
            ],
            Code::GenericTypeArgumentMismatch => &[
                Code::GenericTypeArgumentArity,
                Code::WhereConstraintMismatch,
            ],
            Code::GenericTypeArgumentArity => {
                &[Code::GenericTypeArgumentMismatch, Code::TypeParameterArity]
            }
            Code::TypeParameterArity => &[Code::GenericTypeArgumentArity],
            Code::WhereConstraintMismatch => &[Code::GenericTypeArgumentMismatch],
            // Naming.
            Code::UndefinedVariable => &[Code::UndefinedFunction, Code::UnknownDeclaration],
            Code::UndefinedFunction => &[Code::UnknownBuiltin, Code::UnknownDeclaration],
            Code::UnknownField => &[Code::UnknownMethod, Code::InvalidStructLiteral],
            Code::UnknownMethod => &[Code::UnknownField, Code::CallableExpected],
            Code::UnknownAttribute => {
                &[Code::InvalidAttributeArgument, Code::InvalidAttributeTarget]
            }
            Code::InvalidAttributeArgument => {
                &[Code::UnknownAttribute, Code::InvalidAttributeTarget]
            }
            Code::InvalidAttributeTarget => {
                &[Code::UnknownAttribute, Code::InvalidAttributeArgument]
            }
            // LLM call family — schema, options, provider branching.
            Code::LlmSchemaMissing => &[Code::LlmSchemaInvalid, Code::UnknownLlmOption],
            Code::LlmSchemaInvalid => &[Code::LlmSchemaMissing, Code::UnknownLlmOption],
            Code::UnknownLlmOption => &[Code::DeprecatedLlmOption, Code::LlmSchemaInvalid],
            Code::DeprecatedLlmOption => &[Code::UnknownLlmOption],
            Code::LlmProviderIdentityBranch => &[Code::PromptProviderIdentityBranch],
            // Prompt-template family.
            Code::PromptTemplateParse => &[Code::PromptTargetMissing],
            Code::PromptInjectionRisk => &[Code::LintPromptInjectionRisk],
            Code::PromptProviderIdentityBranch => &[
                Code::LlmProviderIdentityBranch,
                Code::LintTemplateProviderIdentityBranch,
            ],
            Code::PromptVariantExplosion => &[Code::LintTemplateVariantExplosion],
            // Capabilities.
            Code::CapabilityResultUnchecked => {
                &[Code::RescueOutsideFunction, Code::TryOutsideFunction]
            }
            Code::CapabilityUnknownOperation => &[Code::CapabilityCallStaticNameRequired],
            // Recovery / match.
            Code::RescueOutsideFunction => {
                &[Code::TryOutsideFunction, Code::InvalidRescueConstruct]
            }
            Code::TryOutsideFunction => &[Code::RescueOutsideFunction],
            Code::NonExhaustiveMatch => &[Code::InvalidMatchPattern, Code::DuplicateMatchArm],
            Code::DuplicateMatchArm => &[Code::NonExhaustiveMatch, Code::LintDuplicateMatchArm],
            // Module / import family.
            Code::ModuleImportUnresolved => {
                &[Code::ImportResolutionFailed, Code::ImportSymbolMissing]
            }
            Code::ModuleImportUnused => &[Code::LintUnusedImport],
            Code::ImportResolutionFailed => {
                &[Code::ModuleImportUnresolved, Code::ImportSymbolMissing]
            }
            Code::ImportCycle => &[Code::ImportResolutionFailed],
            // Ownership.
            Code::ImmutableAssignment => &[Code::MutableNeverReassigned],
            Code::MutableNeverReassigned => &[Code::LintMutableNeverReassigned],
            // Lint pairs (drift between lint and runtime/typecheck codes).
            Code::LintDeprecatedLlmOptions => &[Code::DeprecatedLlmOption, Code::UnknownLlmOption],
            Code::LintPromptInjectionRisk => &[Code::PromptInjectionRisk],
            Code::LintTemplateVariantExplosion => &[Code::PromptVariantExplosion],
            Code::LintTemplateProviderIdentityBranch => &[Code::PromptProviderIdentityBranch],
            Code::LintRenamedStdlibSymbol => &[Code::DeprecatedStdlibSymbol],
            Code::LintAmbientClockBuiltin => {
                &[Code::InvalidMainSignature, Code::LintRenamedStdlibSymbol]
            }
            Code::LintMutableNeverReassigned => &[Code::MutableNeverReassigned],
            Code::LintUnusedImport => &[Code::ModuleImportUnused],
            Code::LintDuplicateMatchArm => &[Code::DuplicateMatchArm],
            _ => &[],
        }
    }
}

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error returned when parsing an unknown diagnostic code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseCodeError;

impl fmt::Display for ParseCodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("unknown Harn diagnostic code")
    }
}

impl std::error::Error for ParseCodeError {}

impl FromStr for Code {
    type Err = ParseCodeError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Code::ALL
            .iter()
            .copied()
            .find(|code| code.as_str() == value)
            .ok_or(ParseCodeError)
    }
}

/// Autonomy ceiling of a proposed repair.
///
/// Agents and IDEs dispatch on this class to decide whether to auto-apply
/// a fix, propose it as a suggestion, or escalate to a human. Variants
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
            Code::InvalidCast => Some(&REPAIR_CASTS_REMOVE_UNCHECKED),

            // --- NAM / IMP: imports & names -------------------------------
            Code::UndefinedVariable
            | Code::UndefinedFunction
            | Code::UnknownField
            | Code::UnknownMethod
            | Code::UnknownBuiltin
            | Code::UnknownDeclaration => Some(&REPAIR_BINDINGS_RENAME_TO_CLOSEST),
            Code::InvalidMainSignature => Some(&REPAIR_BINDINGS_THREAD_HARNESS),
            Code::DeprecatedFunction => Some(&REPAIR_STDLIB_MIGRATE_RENAMED),
            Code::ModuleImportUnresolved | Code::ImportResolutionFailed => {
                Some(&REPAIR_IMPORTS_FIX_PATH)
            }
            Code::ModuleImportUnused => Some(&REPAIR_IMPORTS_REMOVE_UNUSED),
            Code::ModuleImportOrder => Some(&REPAIR_IMPORTS_REORDER),

            // --- CAP / RCV: capabilities & error recovery -----------------
            Code::CapabilityResultUnchecked => Some(&REPAIR_ERRORS_CHECK_OR_RESCUE),
            Code::CapabilityBindingInvalid => Some(&REPAIR_MANUAL_REVIEW_CAPABILITY),
            Code::RescueOutsideFunction | Code::TryOutsideFunction => {
                Some(&REPAIR_ERRORS_WRAP_IN_FN)
            }

            // --- LLM / PRM: model + prompt contract -----------------------
            Code::DeprecatedLlmOption => Some(&REPAIR_LLM_MIGRATE_DEPRECATED_OPTION),
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
            Code::LintRedundantNilTernary
            | Code::LintUnnecessarySafeNavigation
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
            Code::LintDeprecatedLlmOptions => Some(&REPAIR_LLM_MIGRATE_DEPRECATED_OPTION),
            Code::LintTemplateProviderIdentityBranch => Some(&REPAIR_LLM_USE_CAPABILITY_FLAG),
            Code::LintPromptInjectionRisk => Some(&REPAIR_PROMPTS_ESCAPE_INJECTION),
            Code::LintShadowVariable => Some(&REPAIR_BINDINGS_RENAME_SHADOW),
            Code::LintNamingConvention => Some(&REPAIR_STYLE_RENAME_TO_CONVENTION),
            Code::LintUnhandledApprovalResult => Some(&REPAIR_ERRORS_CHECK_OR_RESCUE),
            Code::LintMissingHarndoc => Some(&REPAIR_DOC_ADD_HARNDOC),
            Code::LintDuplicateMatchArm => Some(&REPAIR_MATCH_REMOVE_DUPLICATE_ARM),
            Code::LintUntypedDictAccess => Some(&REPAIR_TYPES_ADD_SHAPE_ANNOTATION),
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
    summary: "Mark the binding `mut` so it can be reassigned",
    safety: RepairSafety::ScopeLocal,
};

const REPAIR_BINDINGS_MAKE_IMMUTABLE: RepairTemplate = RepairTemplate {
    id: "bindings/make-immutable",
    summary: "Drop `mut` since the binding is never reassigned",
    safety: RepairSafety::BehaviorPreserving,
};

const REPAIR_BINDINGS_RENAME_UNUSED: RepairTemplate = RepairTemplate {
    id: "bindings/rename-unused",
    summary: "Prefix the unused binding with `_` to silence the lint",
    safety: RepairSafety::BehaviorPreserving,
};

const REPAIR_BINDINGS_RENAME_SHADOW: RepairTemplate = RepairTemplate {
    id: "bindings/rename-shadow",
    summary: "Rename the shadowing binding to a distinct name",
    safety: RepairSafety::ScopeLocal,
};

const REPAIR_BINDINGS_THREAD_HARNESS: RepairTemplate = RepairTemplate {
    id: "bindings/thread-harness",
    summary: "Rewrite the entrypoint as `fn main(harness: Harness)` so the runtime can thread its capability handle",
    safety: RepairSafety::SurfaceChanging,
};

const REPAIR_BINDINGS_THREAD_HARNESS_CLOCK: RepairTemplate = RepairTemplate {
    id: "bindings/thread-harness-clock",
    summary: "Replace the ambient clock builtin with the corresponding `harness.clock.*` method",
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

const REPAIR_LLM_MIGRATE_DEPRECATED_OPTION: RepairTemplate = RepairTemplate {
    id: "llm/migrate-deprecated-option",
    summary: "Replace the deprecated option with its supported equivalent",
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

const REPAIR_MANUAL_REVIEW_CAPABILITY: RepairTemplate = RepairTemplate {
    id: "manual/review-capability-binding",
    summary: "Review the capability binding; the fix is not mechanical",
    safety: RepairSafety::NeedsHuman,
};

const REPAIR_MANUAL_NEEDS_HUMAN: RepairTemplate = RepairTemplate {
    id: "manual/needs-human",
    summary: "Plan a human-led change; auto-apply is not safe here",
    safety: RepairSafety::NeedsHuman,
};

/// Every distinct repair template registered by [`Code::repair_template`],
/// in source order. Used by the catalog wire-up in E1.7 and by tests
/// asserting the catalog is healthy.
pub const REPAIR_REGISTRY: &[&RepairTemplate] = &[
    &REPAIR_INSERT_EXPLICIT_CONVERSION,
    &REPAIR_REWRITE_STRING_INTERPOLATION,
    &REPAIR_CASTS_REMOVE_UNCHECKED,
    &REPAIR_CASTS_REMOVE_REDUNDANT,
    &REPAIR_BINDINGS_RENAME_TO_CLOSEST,
    &REPAIR_BINDINGS_MAKE_MUTABLE,
    &REPAIR_BINDINGS_MAKE_IMMUTABLE,
    &REPAIR_BINDINGS_RENAME_UNUSED,
    &REPAIR_BINDINGS_RENAME_SHADOW,
    &REPAIR_BINDINGS_THREAD_HARNESS,
    &REPAIR_BINDINGS_THREAD_HARNESS_CLOCK,
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
    &REPAIR_BLOCK_REMOVE_EMPTY,
    &REPAIR_CONTROL_FLOW_FLATTEN,
    &REPAIR_EXPRESSION_SIMPLIFY,
    &REPAIR_CLONE_REMOVE_REDUNDANT,
    &REPAIR_COLLECTION_PREFER_LAZY,
    &REPAIR_DEAD_CODE_REMOVE,
    &REPAIR_STDLIB_MIGRATE_RENAMED,
    &REPAIR_LLM_MIGRATE_DEPRECATED_OPTION,
    &REPAIR_LLM_ADD_SCHEMA,
    &REPAIR_LLM_USE_CAPABILITY_FLAG,
    &REPAIR_PROMPTS_ESCAPE_INJECTION,
    &REPAIR_PROMPTS_ADD_TOOL_TO_SURFACE,
    &REPAIR_STYLE_RENAME_TO_CONVENTION,
    &REPAIR_TYPES_ADD_SHAPE_ANNOTATION,
    &REPAIR_MANUAL_REVIEW_CAPABILITY,
    &REPAIR_MANUAL_NEEDS_HUMAN,
];

#[cfg(test)]
mod tests {
    use super::{Category, Code, ParseRepairSafetyError, RepairSafety, REPAIR_REGISTRY};
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
}
