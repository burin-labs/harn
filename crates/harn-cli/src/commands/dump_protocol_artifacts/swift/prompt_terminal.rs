//! Swift projection of ACP prompt failure and terminal result contracts.
//!
//! This cluster changes together: the closed terminal enums type both the
//! failed-prompt data and the successful prompt result's Harn extension.

use super::{
    agent_terminal_class_values, agent_terminal_kind_values, agent_terminal_owner_values,
    llm_error_category_values, llm_error_kind_values, llm_error_reason_values, swift_enum,
};

pub(super) fn append_prompt_terminal_types(out: &mut String, prompt_error_schema: &str) {
    out.push_str(&swift_enum(
        "HarnAgentTerminalClass",
        &agent_terminal_class_values(),
    ));
    out.push_str(&swift_enum(
        "HarnAgentTerminalKind",
        &agent_terminal_kind_values(),
    ));
    out.push_str(&swift_enum(
        "HarnAgentTerminalOwner",
        &agent_terminal_owner_values(),
    ));
    out.push_str(&swift_enum(
        "HarnLlmErrorCategory",
        &llm_error_category_values(),
    ));
    out.push_str(&swift_enum("HarnLlmErrorKind", &llm_error_kind_values()));
    out.push_str(&swift_enum(
        "HarnLlmErrorReason",
        &llm_error_reason_values(),
    ));
    out.push_str(&swift_enum(
        "HarnACPPromptErrorSchema",
        &[prompt_error_schema.to_string()],
    ));
    out.push_str(
        r"public struct HarnACPPromptErrorData: Codable, Sendable, Equatable {
    public var schema: HarnACPPromptErrorSchema
    public var terminalClass: HarnAgentTerminalClass
    /// Wire string for `HarnLlmErrorCategory`. Decode with
    /// `HarnLlmErrorCategory(rawValue:)`; `nil` means the producing Harn used
    /// a value this binding predates.
    public var category: String?
    /// Wire string for `HarnLlmErrorKind` (`transient` or `terminal`).
    public var kind: String?
    /// Wire string for `HarnLlmErrorReason`.
    public var reason: String?
    /// PROVIDER PASSTHROUGH. Opaque diagnostic text with no closed set and no
    /// Harn-owned vocabulary. Never branch on it; branch on `reason`.
    public var code: String?
    public var retryable: Bool?
    public var retryAfterMs: Int?
    public var provider: String?
    public var model: String?
}

public struct HarnAgentTerminalOutcome: Codable, Sendable, Equatable {
    public var kind: HarnAgentTerminalKind
    public var reason: String
    public var owner: HarnAgentTerminalOwner
    public var terminalClass: HarnAgentTerminalClass?
    public var message: String?
    public var detail: String?
}

public struct HarnACPPromptResultHarnMetadata: Codable, Sendable, Equatable {
    public var terminal: HarnAgentTerminalOutcome?
}

public struct HarnACPPromptResultMetadata: Codable, Sendable, Equatable {
    public var harn: HarnACPPromptResultHarnMetadata
}

public struct HarnACPPromptResult: Codable, Sendable, Equatable {
    public var stopReason: String
    public var _meta: HarnACPPromptResultMetadata?
}

",
    );
}
