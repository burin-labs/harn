//! Swift projection of ACP prompt failure and terminal result contracts.
//!
//! This cluster changes together: the closed terminal enums type both the
//! failed-prompt data and the successful prompt result's Harn extension.

use super::{
    agent_terminal_class_values, agent_terminal_kind_values, agent_terminal_owner_values,
    swift_enum,
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
        "HarnACPPromptErrorSchema",
        &[prompt_error_schema.to_string()],
    ));
    out.push_str(
        r"public struct HarnACPPromptErrorData: Codable, Sendable, Equatable {
    public var schema: HarnACPPromptErrorSchema
    public var terminalClass: HarnAgentTerminalClass
    public var category: String?
    public var kind: String?
    public var reason: String?
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
