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

#[cfg(test)]
mod tests {
    use super::{Category, Code};
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
}
