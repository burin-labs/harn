//! Swift projection of ACP prompt failure and terminal result contracts.
//!
//! This cluster changes together: the terminal enums type both the
//! failed-prompt data and the successful prompt result's Harn extension.
//!
//! The six vocabularies here are producer-owned and open. They are
//! emitted as open enums so a value a newer Harn introduces
//! reaches a host pinned to an older binding verbatim. As closed `String`
//! raw-value enums they failed to decode, and because
//! `HarnAgentTerminalOutcome` holds `kind` and `owner` non-optionally, one
//! unnamed value cost the host the whole terminal outcome rather than one
//! field.

use super::{
    agent_terminal_class_values, agent_terminal_kind_values, agent_terminal_owner_values,
    llm_error_category_values, llm_error_kind_values, llm_error_reason_values, swift_enum,
    swift_open_enum,
};

pub(super) fn append_prompt_terminal_types(out: &mut String, prompt_error_schema: &str) {
    out.push_str(&swift_open_enum(
        "HarnAgentTerminalClass",
        "Stable terminal classes carried by typed ACP prompt-error data.",
        &agent_terminal_class_values(),
    ));
    out.push_str(&swift_open_enum(
        "HarnAgentTerminalKind",
        "Producer-owned agent terminal outcome kinds.",
        &agent_terminal_kind_values(),
    ));
    out.push_str(&swift_open_enum(
        "HarnAgentTerminalOwner",
        "Owners attributed by producer-owned agent terminal outcomes.",
        &agent_terminal_owner_values(),
    ));
    out.push_str(&swift_open_enum(
        "HarnLlmErrorCategory",
        "Thrown-error categories carried in `category` on the\n         `harn.acp.prompt_error.v1` envelope. Owned by `harn_vm`'s `ErrorCategory`.",
        &llm_error_category_values(),
    ));
    out.push_str(&swift_open_enum(
        "HarnLlmErrorKind",
        "Coarse retry semantics carried in `kind` on the\n         `harn.acp.prompt_error.v1` envelope. Owned by `harn_vm`'s `LlmErrorKind`.\n         `transient` means a byte-identical replay may succeed; `terminal` means it cannot.",
        &llm_error_kind_values(),
    ));
    out.push_str(&swift_open_enum(
        "HarnLlmErrorReason",
        "Canonical provider-failure reason carried in `reason` on the\n         `harn.acp.prompt_error.v1` envelope. Owned by `harn_vm`'s `LlmErrorReason`.\n         The sibling `code` field is a PROVIDER PASSTHROUGH with no closed set: it is\n         opaque diagnostic text, and a host must never branch on it. Branch on `reason`.",
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
