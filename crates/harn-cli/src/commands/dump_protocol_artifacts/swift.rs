use harn_serve::adapters::acp::{
    ACP_PROMPT_ERROR_DATA_SCHEMA, ACP_SCHEMA_COMPATIBILITY, HARN_AGENT_EVENT_KINDS,
    HARN_AGENT_EVENT_METHOD, HARN_CONTENT_EXTENSION_FIELDS, HARN_PROMPT_RESULT_EXTENSION_FIELDS,
    HARN_PROVIDER_CATALOG_METHOD, HARN_SESSION_UPDATE_EXTENSIONS,
    HARN_TOOL_LIFECYCLE_EXTENSION_FIELDS,
};
use harn_serve::MCP_PROTOCOL_VERSION;
use harn_vm::llm::receipts::{TOOL_CALL_RECEIPT_EXECUTORS, TOOL_CALL_RECEIPT_STATUSES};

use super::activity::ActivityVocabulary;
use super::connector_setup::ConnectorSetupVocabulary;
use super::constants::*;
use super::external_action::ExternalActionVocabulary;
use super::external_action_types::append_swift_external_action_types;
use super::prepared_session::append_swift_prepared_session_types;
use super::session_recap::append_swift_session_recap_types;
use super::session_update_payloads::append_swift_session_update_payloads;
use super::support::*;
use super::values::*;

mod activity_types;
mod codegen_support;
mod session_timeline;

use activity_types::append_activity_types;
pub(crate) use codegen_support::*;
use session_timeline::append_session_timeline_types;

#[cfg(test)]
pub(super) fn generate_swift() -> String {
    generate_swift_for_version(
        env!("CARGO_PKG_VERSION"),
        &ExternalActionVocabulary::load_for_tests(),
        &ConnectorSetupVocabulary::load_for_tests(),
        &ActivityVocabulary::load_for_tests(),
    )
}

pub(super) fn generate_swift_for_version(
    artifact_version: &str,
    external_actions: &ExternalActionVocabulary,
    connector_setup: &ConnectorSetupVocabulary,
    activity: &ActivityVocabulary,
) -> String {
    let mut out = generated_header("harn dump-protocol-artifacts", "swift");
    out.push_str("import Foundation\n\n");
    out.push_str("public enum HarnProtocolConstants {\n");
    out.push_str(&format!(
        "    public static let artifactVersion = {}\n",
        json_string_literal(artifact_version)
    ));
    out.push_str(&format!(
        "    public static let acpSchemaCompatibility = {}\n",
        json_string_literal(ACP_SCHEMA_COMPATIBILITY)
    ));
    out.push_str("    public static let toolPermissionDecisionSchema = \"harn.tool_permission_decision.v1\"\n");
    out.push_str("    public static let toolPermissionActivitySchema = \"harn.tool_permission_activity.v1\"\n");
    out.push_str("    public static let externalActionActivitySchema = \"harn.external_action_activity.v1\"\n");
    out.push_str(
        "    public static let externalActionReceiptSchema = \"harn.external_action_receipt.v1\"\n",
    );
    out.push_str(&format!(
        "    public static let harnAgentEventMethod = {}\n",
        json_string_literal(HARN_AGENT_EVENT_METHOD)
    ));
    out.push_str(&format!(
        "    public static let harnProviderCatalogMethod = {}\n",
        json_string_literal(HARN_PROVIDER_CATALOG_METHOD)
    ));
    out.push_str(&format!(
        "    public static let acpPromptErrorDataSchema = {}\n",
        json_string_literal(ACP_PROMPT_ERROR_DATA_SCHEMA)
    ));
    for (name, value) in [
        ("mcpProtocolVersion", MCP_PROTOCOL_VERSION),
        (
            "mcpJsonSchema202012Dialect",
            MCP_JSON_SCHEMA_2020_12_DIALECT,
        ),
    ] {
        out.push_str(&format!(
            "    public static let {name} = {}\n",
            json_string_literal(value)
        ));
    }
    out.push_str(&format!(
        "    public static let mcpUnsupportedProtocolVersionErrorCode = {MCP_UNSUPPORTED_PROTOCOL_VERSION_ERROR_CODE}\n"
    ));
    out.push_str(&format!(
        "    public static let mcpUnsupportedProtocolVersionErrorMessage = {}\n",
        json_string_literal(MCP_UNSUPPORTED_PROTOCOL_VERSION_ERROR_MESSAGE)
    ));
    out.push_str(&swift_string_array(
        "mcpProtocolVersions",
        MCP_PROTOCOL_VERSIONS,
    ));
    out.push_str(&swift_string_array(
        "mcpRequiredMetadataKeys",
        MCP_REQUIRED_METADATA_KEYS,
    ));
    out.push_str(&swift_string_array("mcpMetadataKeys", MCP_METADATA_KEYS));
    out.push_str(&swift_string_array(
        "mcpStandardHTTPHeaders",
        MCP_STANDARD_HTTP_HEADERS,
    ));
    out.push_str(&swift_string_array(
        "mcpCacheResultFields",
        MCP_CACHE_RESULT_FIELDS,
    ));
    out.push_str(&swift_string_array(
        "mcpOAuthClientRegistrationModes",
        MCP_OAUTH_CLIENT_REGISTRATION_MODES,
    ));
    out.push_str(&swift_string_array(
        "mcpOAuthAuthModes",
        MCP_OAUTH_AUTH_MODES,
    ));
    out.push_str(&swift_string_array(
        "mcpOAuthApplicationTypes",
        MCP_OAUTH_APPLICATION_TYPES,
    ));
    out.push_str(&swift_string_array(
        "acpSessionUpdateExtensions",
        HARN_SESSION_UPDATE_EXTENSIONS,
    ));
    out.push_str(&swift_string_array(
        "harnAgentEventKinds",
        HARN_AGENT_EVENT_KINDS,
    ));
    out.push_str(&swift_string_array(
        "toolLifecycleExtensionFields",
        HARN_TOOL_LIFECYCLE_EXTENSION_FIELDS,
    ));
    out.push_str(&swift_string_array(
        "contentExtensionFields",
        HARN_CONTENT_EXTENSION_FIELDS,
    ));
    out.push_str(&swift_string_array(
        "promptResultExtensionFields",
        HARN_PROMPT_RESULT_EXTENSION_FIELDS,
    ));
    out.push_str(&swift_string_array(
        "toolCallReceiptStatuses",
        TOOL_CALL_RECEIPT_STATUSES,
    ));
    out.push_str(&swift_string_array(
        "toolCallReceiptExecutors",
        TOOL_CALL_RECEIPT_EXECUTORS,
    ));
    out.push_str("}\n\n");

    out.push_str(&swift_enum(
        "HarnExternalActionOutcome",
        &external_actions.outcomes,
    ));
    out.push_str(&swift_enum(
        "HarnExternalActionReceiptStatus",
        &external_actions.receipt_statuses,
    ));
    out.push_str(&swift_enum(
        "HarnExternalActionNextAction",
        &external_actions.next_actions,
    ));
    out.push_str(&swift_enum(
        "HarnExternalActionEnvironment",
        &external_actions.environments,
    ));
    out.push_str(&swift_enum(
        "HarnExternalActionAuthorizationMethod",
        &external_actions.authorization_methods,
    ));
    out.push_str(&swift_enum(
        "HarnExternalActionAuthenticationAssurance",
        &external_actions.authentication_assurances,
    ));
    out.push_str(&swift_enum(
        "HarnExternalActionDisclosureSource",
        &external_actions.disclosure_sources,
    ));
    out.push_str(&swift_enum(
        "HarnExternalActionErrorKind",
        &external_actions.error_kinds,
    ));
    out.push_str(&swift_enum(
        "HarnExternalActionProtectedFieldClass",
        &external_actions.protected_field_classes,
    ));
    out.push_str(&swift_enum(
        "HarnExternalActionPassengerGender",
        &external_actions.passenger_genders,
    ));
    out.push_str(&swift_enum(
        "HarnExternalActionActivityStatus",
        &external_actions.activity_statuses,
    ));
    out.push_str("public extension HarnExternalActionActivityStatus {\n");
    out.push_str(
        "    /// Whether this snapshot is a final outcome and may only replay identically.\n",
    );
    out.push_str("    var isTerminal: Bool {\n        switch self {\n");
    for value in &external_actions.terminal_activity_statuses {
        out.push_str("        case .");
        out.push_str(&swift_case_name(value));
        out.push_str(": true\n");
    }
    out.push_str("        default: false\n        }\n    }\n}\n\n");
    out.push_str("public extension HarnExternalActionActivityStatus {\n");
    out.push_str("    /// Whether a later snapshot may advance from this lifecycle status.\n");
    out.push_str("    func canAdvance(to next: Self) -> Bool {\n");
    out.push_str("        if isTerminal { return self == next }\n");
    out.push_str("        if next.isTerminal { return true }\n");
    out.push_str("        return progressRank <= next.progressRank\n    }\n\n");
    out.push_str("    private var progressRank: Int {\n        switch self {\n");
    for (index, value) in external_actions
        .progress_activity_statuses
        .iter()
        .enumerate()
    {
        out.push_str("        case .");
        out.push_str(&swift_case_name(value));
        out.push_str(&format!(": {index}\n"));
    }
    out.push_str("        default: Int.max\n        }\n    }\n}\n\n");
    out.push_str(&swift_enum(
        "HarnExternalActionPolicyLayer",
        &external_actions.policy_layers,
    ));
    out.push_str(&swift_enum(
        "HarnExternalActionPolicyEvaluationOutcome",
        &external_actions.policy_evaluation_outcomes,
    ));
    out.push_str(&swift_enum(
        "HarnExternalActionDecisionOutcome",
        &external_actions.decision_outcomes,
    ));
    out.push_str(&swift_enum(
        "HarnExternalActionDecider",
        &external_actions.deciders,
    ));
    out.push_str(&swift_enum(
        "HarnExternalActionReconciliationStatus",
        &external_actions.reconciliation_statuses,
    ));
    append_swift_external_action_types(&mut out);
    append_activity_types(&mut out, activity);
    out.push_str(&swift_enum(
        "HarnConnectorSetupStage",
        &connector_setup.stages,
    ));
    out.push_str(&swift_enum(
        "HarnConnectorSetupStatus",
        &connector_setup.statuses,
    ));
    out.push_str(&swift_enum(
        "HarnConnectorSetupInteraction",
        &connector_setup.interactions,
    ));
    out.push_str(&swift_enum(
        "HarnConnectorSetupConfigurationField",
        &connector_setup.configuration_fields,
    ));
    out.push_str(&swift_enum(
        "HarnConnectorSetupErrorCode",
        &connector_setup.error_codes,
    ));
    out.push_str(
        "public struct HarnConnectorSetupEvent: Codable, Sendable, Equatable {\n\
         \x20   public let schema: String\n\
         \x20   public let sequence: Int\n\
         \x20   public let connector: String\n\
         \x20   public let stage: HarnConnectorSetupStage\n\
         \x20   public let status: HarnConnectorSetupStatus\n\
         \x20   public let interaction: HarnConnectorSetupInteraction\n\
         \x20   public let message: String\n\
         \x20   public let errorCode: HarnConnectorSetupErrorCode?\n\
         \x20   public let recovery: String?\n\n\
         \x20   enum CodingKeys: String, CodingKey {\n\
         \x20       case schema, sequence, connector, stage, status, interaction, message, recovery\n\
         \x20       case errorCode = \"error_code\"\n\
         \x20   }\n\
         }\n\n",
    );

    for vocabulary in acp_method_vocabularies() {
        out.push_str(&swift_enum_with_deprecations(
            vocabulary.swift_enum_name,
            &vocabulary.values,
            vocabulary.deprecated_values,
        ));
    }
    out.push_str(&swift_enum(
        "HarnACPAgentNotification",
        &strs_to_strings(ACP_AGENT_NOTIFICATIONS),
    ));
    out.push_str(&swift_enum(
        "HarnACPSessionUpdate",
        &all_acp_session_updates(),
    ));
    out.push_str(&swift_enum(
        "HarnACPContentBlockType",
        &strs_to_strings(ACP_CONTENT_BLOCK_TYPES),
    ));
    out.push_str(&swift_enum("HarnACPToolKind", &tool_kind_values()));
    out.push_str(&swift_enum(
        "HarnACPToolCallStatus",
        &tool_call_status_values(),
    ));
    out.push_str(&swift_enum(
        "HarnToolCallErrorCategory",
        &tool_call_error_category_values(),
    ));
    out.push_str(&swift_enum(
        "HarnToolMutationStatus",
        &tool_mutation_status_values(),
    ));
    out.push_str(&swift_enum(
        "HarnSideEffectLevel",
        &side_effect_level_values(),
    ));
    out.push_str(&swift_enum(
        "HarnCompletionEvidenceRole",
        &completion_evidence_role_values(),
    ));
    out.push_str(&swift_enum("HarnWorkerStatus", &worker_status_values()));
    out.push_str(&swift_enum(
        "HarnAgentLifecycleState",
        &agent_lifecycle_state_values(),
    ));
    out.push_str(&swift_enum(
        "HarnAgentLifecycleEvent",
        &agent_lifecycle_event_values(),
    ));
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
        &[ACP_PROMPT_ERROR_DATA_SCHEMA.to_string()],
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
    out.push_str(&swift_enum(
        "HarnToolCallReceiptStatus",
        &strs_to_strings(TOOL_CALL_RECEIPT_STATUSES),
    ));
    out.push_str(&swift_enum(
        "HarnToolCallReceiptExecutor",
        &strs_to_strings(TOOL_CALL_RECEIPT_EXECUTORS),
    ));
    out.push_str(&swift_enum(
        "HarnA2ATaskState",
        &strs_to_strings(A2A_TASK_STATES),
    ));
    out.push_str(&swift_enum(
        "HarnA2ATaskEventType",
        &strs_to_strings(A2A_TASK_EVENT_TYPES),
    ));
    out.push_str(&swift_enum("HarnMCPMethod", &strs_to_strings(MCP_METHODS)));
    out.push_str(&swift_enum(
        "HarnMCPCacheScope",
        &strs_to_strings(MCP_CACHE_SCOPES),
    ));
    out.push_str(&swift_enum(
        "HarnMCPResultType",
        &strs_to_strings(MCP_RESULT_TYPES),
    ));
    out.push_str(&swift_enum(
        "HarnMCPLoggingLevel",
        &strs_to_strings(MCP_LOGGING_LEVELS),
    ));
    out.push_str(&swift_enum(
        "HarnMCPOAuthClientRegistrationMode",
        &strs_to_strings(MCP_OAUTH_CLIENT_REGISTRATION_MODES),
    ));
    out.push_str(&swift_enum(
        "HarnMCPOAuthAuthMode",
        &strs_to_strings(MCP_OAUTH_AUTH_MODES),
    ));
    out.push_str(&swift_enum(
        "HarnMCPOAuthApplicationType",
        &strs_to_strings(MCP_OAUTH_APPLICATION_TYPES),
    ));

    out.push_str(
        r#"public enum HarnACPValue: Codable, Sendable, Equatable {
    case null
    case bool(Bool)
    case int(Int)
    case double(Double)
    case string(String)
    case array([HarnACPValue])
    case object([String: HarnACPValue])

    public init?(jsonEncodable value: Encodable) {
        let encoder = JSONEncoder()
        guard let data = try? encoder.encode(HarnAnyEncodable(value)),
              let object = try? JSONSerialization.jsonObject(with: data),
              let converted = HarnACPValue(jsonObject: object) else {
            return nil
        }
        self = converted
    }

    public init?(jsonObject: Any) {
        if let scalar = Self.jsonScalar(jsonObject) {
            self = scalar
        } else if let values = jsonObject as? [Any] {
            guard let array = Self.jsonArray(values) else { return nil }
            self = array
        } else if let values = jsonObject as? [String: Any] {
            guard let object = Self.jsonDictionary(values) else { return nil }
            self = object
        } else {
            return nil
        }
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        if container.decodeNil() {
            self = .null
        } else if let value = try? container.decode(Bool.self) {
            self = .bool(value)
        } else if let value = try? container.decode(Int.self) {
            self = .int(value)
        } else if let value = try? container.decode(Double.self) {
            self = .double(value)
        } else if let value = try? container.decode(String.self) {
            self = .string(value)
        } else if let value = try? container.decode([HarnACPValue].self) {
            self = .array(value)
        } else if let value = try? container.decode([String: HarnACPValue].self) {
            self = .object(value)
        } else {
            self = .null
        }
    }

    public func encode(to encoder: Encoder) throws {
        var container = encoder.singleValueContainer()
        switch self {
        case .null: try container.encodeNil()
        case .bool(let value): try container.encode(value)
        case .int(let value): try container.encode(value)
        case .double(let value): try container.encode(value)
        case .string(let value): try container.encode(value)
        case .array(let value): try container.encode(value)
        case .object(let value): try container.encode(value)
        }
    }

    public var displayString: String {
        switch self {
        case .null: return "nil"
        case .bool(let value): return value ? "true" : "false"
        case .int(let value): return "\(value)"
        case .double(let value): return "\(value)"
        case .string(let value): return value
        case .array(let value): return "[\(value.map(\.displayString).joined(separator: ", "))]"
        case .object(let value):
            let pairs = value.sorted(by: { $0.key < $1.key })
                .map { "\($0.key): \($0.value.displayString)" }
            return "{\(pairs.joined(separator: ", "))}"
        }
    }

    public var stringValue: String? {
        if case .string(let value) = self { return value }
        return nil
    }

    public var intValue: Int? {
        if case .int(let value) = self { return value }
        if case .double(let value) = self, value.rounded() == value { return Int(value) }
        return nil
    }

    public var boolValue: Bool? {
        if case .bool(let value) = self { return value }
        return nil
    }

    public var arrayValue: [HarnACPValue]? {
        if case .array(let value) = self { return value }
        return nil
    }

    public var objectValue: [String: HarnACPValue]? {
        if case .object(let value) = self { return value }
        return nil
    }

    public subscript(_ key: String) -> HarnACPValue? {
        objectValue?[key]
    }

    private static func jsonScalar(_ jsonObject: Any) -> HarnACPValue? {
        switch jsonObject {
        case _ as NSNull: return .null
        case let value as NSNumber: return jsonNumber(value)
        case let value as String: return .string(value)
        default: return nil
        }
    }

    private static func jsonSignedInteger(_ value: Int64) -> HarnACPValue? {
        guard value <= Int64(Int.max), value >= Int64(Int.min) else { return nil }
        return .int(Int(value))
    }

    private static func jsonUnsignedInteger(_ value: UInt64) -> HarnACPValue {
        value <= UInt64(Int.max) ? .int(Int(value)) : .double(Double(value))
    }

    private static func jsonNumber(_ value: NSNumber) -> HarnACPValue? {
        #if canImport(Darwin)
        if CFGetTypeID(value) == CFBooleanGetTypeID() {
            return .bool(value.boolValue)
        }
        if CFNumberIsFloatType(value) {
            return .double(value.doubleValue)
        }
        #endif
        let objCType = String(cString: value.objCType)
        #if !canImport(Darwin)
        if objCType == "c" || objCType == "B" {
            return .bool(value.boolValue)
        }
        #endif
        if objCType == "f" || objCType == "d" {
            return .double(value.doubleValue)
        }
        if ["q", "l", "i", "s"].contains(objCType) {
            return jsonSignedInteger(value.int64Value)
        }
        if ["Q", "L", "I", "S"].contains(objCType) {
            return jsonUnsignedInteger(value.uint64Value)
        }
        return jsonSignedInteger(value.int64Value)
    }

    private static func jsonArray(_ values: [Any]) -> HarnACPValue? {
        var items: [HarnACPValue] = []
        items.reserveCapacity(values.count)
        for value in values {
            guard let item = HarnACPValue(jsonObject: value) else { return nil }
            items.append(item)
        }
        return .array(items)
    }

    private static func jsonDictionary(_ values: [String: Any]) -> HarnACPValue? {
        var fields: [String: HarnACPValue] = [:]
        fields.reserveCapacity(values.count)
        for (key, value) in values {
            guard let item = HarnACPValue(jsonObject: value) else { return nil }
            fields[key] = item
        }
        return .object(fields)
    }
}

public typealias HarnACPObject = [String: HarnACPValue]

private struct HarnAnyEncodable: Encodable {
    let value: Encodable

    init(_ value: Encodable) {
        self.value = value
    }

    func encode(to encoder: Encoder) throws {
        try value.encode(to: encoder)
    }
}

public enum HarnJsonRpcId: Codable, Sendable, Hashable, ExpressibleByIntegerLiteral, ExpressibleByStringLiteral {
    case null
    case int(Int)
    case string(String)

    public init(integerLiteral value: Int) {
        self = .int(value)
    }

    public init(stringLiteral value: String) {
        self = .string(value)
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        if container.decodeNil() {
            self = .null
        } else if let value = try? container.decode(Int.self) {
            self = .int(value)
        } else if let value = try? container.decode(String.self) {
            self = .string(value)
        } else {
            throw DecodingError.typeMismatch(
                HarnJsonRpcId.self,
                DecodingError.Context(
                    codingPath: decoder.codingPath,
                    debugDescription: "JSON-RPC id must be an integer, string, or null"
                )
            )
        }
    }

    public func encode(to encoder: Encoder) throws {
        var container = encoder.singleValueContainer()
        switch self {
        case .null: try container.encodeNil()
        case .int(let value): try container.encode(value)
        case .string(let value): try container.encode(value)
        }
    }

    public var intValue: Int? {
        if case .int(let value) = self { return value }
        return nil
    }

    public var stringValue: String? {
        if case .string(let value) = self { return value }
        return nil
    }
}

public struct HarnACPRequest: Codable, Sendable, Equatable {
    public let jsonrpc: String
    public var id: HarnJsonRpcId
    public var method: String
    public var params: HarnACPValue?

    public init(id: HarnJsonRpcId, method: String, params: HarnACPValue? = nil) {
        self.jsonrpc = "2.0"
        self.id = id
        self.method = method
        self.params = params
    }

    public init(id: Int, method: String, params: HarnACPValue? = nil) {
        self.init(id: .int(id), method: method, params: params)
    }

    public init(id: String, method: String, params: HarnACPValue? = nil) {
        self.init(id: .string(id), method: method, params: params)
    }
}

public struct HarnACPError: Codable, Sendable, Equatable {
    public var code: Int
    public var message: String
    public var data: HarnACPValue?

    public init(code: Int, message: String, data: HarnACPValue? = nil) {
        self.code = code
        self.message = message
        self.data = data
    }
}

public struct HarnACPResponse: Codable, Sendable, Equatable {
    public let jsonrpc: String
    public var id: HarnJsonRpcId
    public var result: HarnACPValue?
    public var error: HarnACPError?

    public init(
        jsonrpc: String = "2.0",
        id: HarnJsonRpcId,
        result: HarnACPValue? = nil,
        error: HarnACPError? = nil
    ) {
        self.jsonrpc = jsonrpc
        self.id = id
        self.result = result
        self.error = error
    }

    public static func success(id: HarnJsonRpcId, result: HarnACPValue) -> HarnACPResponse {
        HarnACPResponse(id: id, result: result)
    }

    public static func success(id: Int, result: HarnACPValue) -> HarnACPResponse {
        success(id: .int(id), result: result)
    }

    public static func error(
        id: HarnJsonRpcId,
        code: Int,
        message: String,
        data: HarnACPValue? = nil
    ) -> HarnACPResponse {
        HarnACPResponse(id: id, error: HarnACPError(code: code, message: message, data: data))
    }

    public static func error(
        id: Int,
        code: Int,
        message: String,
        data: HarnACPValue? = nil
    ) -> HarnACPResponse {
        error(id: .int(id), code: code, message: message, data: data)
    }
}

public struct HarnACPNotification: Codable, Sendable, Equatable {
    public let jsonrpc: String
    public var method: String
    public var params: HarnACPValue?

    public init(method: String, params: HarnACPValue? = nil) {
        self.jsonrpc = "2.0"
        self.method = method
        self.params = params
    }
}

public struct HarnACPExtensionMeta: Codable, Sendable, Equatable {
    public var harn: HarnACPObject?
}

public struct HarnACPContentBlock: Codable, Sendable, Equatable {
    public var type: String
    public var text: String?
    public var meta: HarnACPExtensionMeta?

    enum CodingKeys: String, CodingKey {
        case type
        case text
        case meta = "_meta"
    }
}

public enum HarnACPToolExecutor: Codable, Sendable, Equatable {
    case harnBuiltin
    case hostBridge
    case providerNative
    case mcpServer(name: String)
    case unknown(String)

    enum ObjectKey: String, CodingKey {
        case kind
        case serverName
    }

    public init(from decoder: Decoder) throws {
        if let raw = try? decoder.singleValueContainer().decode(String.self) {
            switch raw {
            case "harn_builtin": self = .harnBuiltin
            case "host_bridge": self = .hostBridge
            case "provider_native": self = .providerNative
            default: self = .unknown(raw)
            }
            return
        }
        let object = try decoder.container(keyedBy: ObjectKey.self)
        let kind = try object.decode(String.self, forKey: .kind)
        if kind == "mcp_server" {
            self = .mcpServer(name: try object.decode(String.self, forKey: .serverName))
        } else {
            self = .unknown(kind)
        }
    }

    public func encode(to encoder: Encoder) throws {
        switch self {
        case .harnBuiltin:
            var container = encoder.singleValueContainer()
            try container.encode("harn_builtin")
        case .hostBridge:
            var container = encoder.singleValueContainer()
            try container.encode("host_bridge")
        case .providerNative:
            var container = encoder.singleValueContainer()
            try container.encode("provider_native")
        case .mcpServer(let name):
            var container = encoder.container(keyedBy: ObjectKey.self)
            try container.encode("mcp_server", forKey: .kind)
            try container.encode(name, forKey: .serverName)
        case .unknown(let raw):
            var container = encoder.singleValueContainer()
            try container.encode(raw)
        }
    }

    public var displayLabel: String {
        switch self {
        case .harnBuiltin: return "harn_builtin"
        case .hostBridge: return "host_bridge"
        case .providerNative: return "provider_native"
        case .mcpServer(let name): return "mcp:\(name)"
        case .unknown(let raw): return raw
        }
    }
}

public struct HarnToolLifecycleMeta: Codable, Sendable, Equatable {
    public var audit: HarnACPValue?
    public var changedPaths: [String]?
    public var data: HarnACPValue?
    public var durationMs: Double?
    public var error: String?
    public var errorCategory: HarnToolCallErrorCategory?
    public var executionDurationMs: Double?
    public var executor: HarnACPToolExecutor?
    public var mutationStatus: HarnToolMutationStatus?
    public var parsing: Bool?
    public var rawInputPartial: String?
}

public enum HarnHostInjectionKind: String, Codable, Sendable, Equatable {
    case hostToolResult = "host_tool_result"
    case hostAttachment = "host_attachment"
}

public enum HarnHostInjectionDelivery: String, Codable, Sendable, Equatable {
    case turnBoundary = "turn_boundary"
    case immediate
    case afterNextToolCall = "after_next_tool_call"
}

public struct HarnHostInjectionProvenance: Codable, Sendable, Equatable {
    public var initiator: String
    public var source: String
    public var host: String?
    public var tsMs: Int

    enum CodingKeys: String, CodingKey {
        case initiator, source, host
        case tsMs = "ts_ms"
    }
}

public struct HarnHostInjectionEvent: Codable, Sendable, Equatable {
    public var kind: HarnHostInjectionKind
    public var delivery: HarnHostInjectionDelivery?
    public var payload: [String: HarnACPValue]
    public var provenance: HarnHostInjectionProvenance
}

public struct HarnACPSessionInjectHostEventParams: Codable, Sendable, Equatable {
    public var sessionId: String
    public var event: HarnHostInjectionEvent
}

public enum HarnPlanCommentState: String, Codable, Sendable, Equatable {
    case open
    case addressed
    case resolved
    case reopened
}

public enum HarnPlanApprovalState: String, Codable, Sendable, Equatable {
    case unrequested
    case requested
    case approved
    case rejected
}

public struct HarnPlanAuthor: Codable, Sendable, Equatable {
    public var id: String
    public var displayName: String?

    enum CodingKeys: String, CodingKey {
        case id
        case displayName = "display_name"
    }
}

public struct HarnPlanSource: Codable, Sendable, Equatable {
    public var kind: String
    public var uri: String?
}

public struct HarnPlanStep: Codable, Sendable, Equatable {
    public var id: String
    public var content: String
    public var status: String
    public var priority: HarnACPValue?
}

public struct HarnPlanApproval: Codable, Sendable, Equatable {
    public var state: HarnPlanApprovalState
    public var requestId: String?
    public var reviewer: String?
    public var reviewers: [String]?
    public var approvedAt: String?
    public var reason: String?

    enum CodingKeys: String, CodingKey {
        case state
        case requestId = "request_id"
        case reviewer
        case reviewers
        case approvedAt = "approved_at"
        case reason
    }
}

public struct HarnPlanArtifact: Codable, Sendable, Equatable {
    public var type: String
    public var schemaVersion: String
    public var id: String
    public var tool: String
    public var title: String
    public var summary: String
    public var steps: [HarnPlanStep]
    public var assumptions: [String]
    public var openQuestions: [String]
    public var verificationCommands: [String]
    public var approval: HarnPlanApproval

    enum CodingKeys: String, CodingKey {
        case type = "_type"
        case schemaVersion = "schema_version"
        case id
        case tool
        case title
        case summary
        case steps
        case assumptions
        case openQuestions = "open_questions"
        case verificationCommands = "verification_commands"
        case approval
    }
}

public struct HarnPlanRevisionOperation: Codable, Sendable, Equatable {
    public var kind: String
    public var eventId: String
    public var commentId: String?
    public var state: HarnPlanCommentState?

    enum CodingKeys: String, CodingKey {
        case kind
        case eventId = "event_id"
        case commentId = "comment_id"
        case state
    }
}

public struct HarnPlanRevision: Codable, Sendable, Equatable {
    public var revisionId: String
    public var parentRevisionId: String?
    public var markdown: String
    public var plan: HarnPlanArtifact
    public var author: HarnPlanAuthor
    public var source: HarnPlanSource
    public var createdAt: String
    public var operation: HarnPlanRevisionOperation

    enum CodingKeys: String, CodingKey {
        case revisionId = "revision_id"
        case parentRevisionId = "parent_revision_id"
        case markdown
        case plan
        case author
        case source
        case createdAt = "created_at"
        case operation
    }
}

public struct HarnPlanTextRange: Codable, Sendable, Equatable {
    public var start: Int
    public var end: Int
}

public struct HarnPlanCommentAnchor: Codable, Sendable, Equatable {
    public var stepId: String?
    public var quotedText: String?
    public var range: HarnPlanTextRange?

    enum CodingKeys: String, CodingKey {
        case stepId = "step_id"
        case quotedText = "quoted_text"
        case range
    }
}

public struct HarnPlanComment: Codable, Sendable, Equatable {
    public var commentId: String
    public var anchor: HarnPlanCommentAnchor
    public var body: String
    public var state: HarnPlanCommentState
    public var author: HarnPlanAuthor
    public var createdAt: String
    public var updatedAt: String

    enum CodingKeys: String, CodingKey {
        case commentId = "comment_id"
        case anchor
        case body
        case state
        case author
        case createdAt = "created_at"
        case updatedAt = "updated_at"
    }
}

public struct HarnPlanCommentResolutionReceipt: Codable, Sendable, Equatable {
    public var receiptId: String
    public var commentId: String
    public var inputRevisionId: String
    public var outputRevisionId: String
    public var agentRunId: String
    public var eventId: String
    public var explanation: String?
    public var createdAt: String

    enum CodingKeys: String, CodingKey {
        case receiptId = "receipt_id"
        case commentId = "comment_id"
        case inputRevisionId = "input_revision_id"
        case outputRevisionId = "output_revision_id"
        case agentRunId = "agent_run_id"
        case eventId = "event_id"
        case explanation
        case createdAt = "created_at"
    }
}

public struct HarnPlanDocument: Codable, Sendable, Equatable {
    public var type: String
    public var schemaVersion: String
    public var documentId: String
    public var currentRevision: HarnPlanRevision
    public var comments: [HarnPlanComment]
    public var resolutionReceipts: [HarnPlanCommentResolutionReceipt]
    public var createdAt: String
    public var updatedAt: String

    enum CodingKeys: String, CodingKey {
        case type = "_type"
        case schemaVersion = "schema_version"
        case documentId = "document_id"
        case currentRevision = "current_revision"
        case comments
        case resolutionReceipts = "resolution_receipts"
        case createdAt = "created_at"
        case updatedAt = "updated_at"
    }
}

public struct HarnToolCallReceipt: Codable, Sendable, Equatable {
    public var schemaVersion: Int
    public var sessionId: String
    public var runId: String?
    public var toolCallId: String
    public var toolName: String
    public var iteration: Int
    public var turnIndex: Int?
    public var emitOrder: Int
    public var reason: String?
    public var kind: String?
    public var executor: HarnToolCallReceiptExecutor?
    public var status: HarnToolCallReceiptStatus
    public var errorCategory: String?
    public var durationMs: Int
    public var argsHash: String
    public var resultHash: String?
    public var audit: HarnACPValue
    public var emittedAt: String
    public var model: String?
    public var provider: String?

    enum CodingKeys: String, CodingKey {
        case schemaVersion = "schema_version"
        case sessionId = "session_id"
        case runId = "run_id"
        case toolCallId = "tool_call_id"
        case toolName = "tool_name"
        case iteration
        case turnIndex = "turn_index"
        case emitOrder = "emit_order"
        case reason
        case kind
        case executor
        case status
        case errorCategory = "error_category"
        case durationMs = "duration_ms"
        case argsHash = "args_hash"
        case resultHash = "result_hash"
        case audit
        case emittedAt = "emitted_at"
        case model
        case provider
    }
}

public struct HarnACPToolCall: Codable, Sendable, Equatable {
    public var sessionUpdate: HarnACPSessionUpdate
    public var toolCallId: String
    public var title: String
    public var kind: HarnACPToolKind?
    public var status: HarnACPToolCallStatus?
    public var content: [HarnACPContentBlock]?
    public var locations: [HarnACPValue]?
    public var rawInput: HarnACPValue?
    public var rawOutput: HarnACPValue?
    public var meta: HarnACPExtensionMeta?

    enum CodingKeys: String, CodingKey {
        case sessionUpdate
        case toolCallId
        case title
        case kind
        case status
        case content
        case locations
        case rawInput
        case rawOutput
        case meta = "_meta"
    }
}

public struct HarnACPSessionUpdateEnvelope: Codable, Sendable, Equatable {
    public var sessionUpdate: HarnACPSessionUpdate
    public var content: HarnACPValue?
    public var messageId: String?
    public var entries: [HarnACPValue]?
    public var harnPlanDocument: HarnPlanDocument?
    public var keptTurnCount: Int?
    public var removedTurnCount: Int?
    public var newTipTurnId: String?
    public var reason: String?
    public var toolCallId: String?
    public var title: String?
    public var kind: HarnACPToolKind?
    public var status: HarnACPToolCallStatus?
    public var rawInput: HarnACPValue?
    public var rawOutput: HarnACPValue?
    public var meta: HarnACPExtensionMeta?

    enum CodingKeys: String, CodingKey {
        case sessionUpdate
        case content
        case messageId
        case entries
        case harnPlanDocument
        case keptTurnCount
        case removedTurnCount
        case newTipTurnId
        case reason
        case toolCallId
        case title
        case kind
        case status
        case rawInput
        case rawOutput
        case meta = "_meta"
    }
}

public struct HarnACPSessionUpdateParams: Codable, Sendable, Equatable {
    public var sessionId: String
    public var update: HarnACPSessionUpdateEnvelope
}

public struct HarnACPSessionUpdateNotification: Codable, Sendable, Equatable {
    public let jsonrpc: String
    public var method: String
    public var params: HarnACPSessionUpdateParams
}

public struct HarnAgentEventNotification: Codable, Sendable, Equatable {
    public let jsonrpc: String
    public var method: String
    public var params: HarnACPObject
}

public struct HarnPromptCapabilities: Codable, Sendable, Equatable {
    public var image: Bool?
    public var audio: Bool?
    public var embeddedContext: Bool?
}

public struct HarnACPAgentCapabilities: Codable, Sendable, Equatable {
    public var meta: HarnACPExtensionMeta?
    public var loadSession: Bool?
    public var session: HarnACPObject?
    public var promptCapabilities: HarnPromptCapabilities?
    public var mcpCapabilities: HarnACPObject?
    public var sessionCapabilities: HarnACPObject?

    enum CodingKeys: String, CodingKey {
        case meta = "_meta"
        case loadSession
        case session
        case promptCapabilities
        case mcpCapabilities
        case sessionCapabilities
    }
}

public struct HarnToolArgSchema: Codable, Sendable, Equatable {
    public var pathParams: [String]
    public var dependencyKeyParams: [String]
    public var dependencyRangeParams: [[String: String]]
    public var argAliases: [String: String]
    public var required: [String]

    enum CodingKeys: String, CodingKey {
        case pathParams = "path_params"
        case dependencyKeyParams = "dependency_key_params"
        case dependencyRangeParams = "dependency_range_params"
        case argAliases = "arg_aliases"
        case required
    }
}

public struct HarnToolAnnotations: Codable, Sendable, Equatable {
    public var kind: HarnACPToolKind
    public var sideEffectLevel: HarnSideEffectLevel
    public var completionEvidenceRole: HarnCompletionEvidenceRole?
    public var argSchema: HarnToolArgSchema
    public var capabilities: [String: [String]]
    public var emitsArtifacts: Bool
    public var resultReaders: [String]
    public var inlineResult: Bool

    enum CodingKeys: String, CodingKey {
        case kind
        case sideEffectLevel = "side_effect_level"
        case completionEvidenceRole = "completion_evidence_role"
        case argSchema = "arg_schema"
        case capabilities
        case emitsArtifacts = "emits_artifacts"
        case resultReaders = "result_readers"
        case inlineResult = "inline_result"
    }
}

public struct HarnA2ATaskStatus: Codable, Sendable, Equatable {
    public var state: HarnA2ATaskState
    public var message: HarnACPValue?
    public var timestamp: String?
}

public struct HarnA2ATask: Codable, Sendable, Equatable {
    public var id: String
    public var contextId: String?
    public var status: HarnA2ATaskStatus
    public var history: [HarnACPValue]?
    public var artifacts: [HarnACPValue]?
    public var metadata: HarnACPObject?
}

public typealias HarnMCPJsonSchema202012 = HarnACPObject

public struct HarnMCPImplementation: Codable, Sendable, Equatable {
    public var name: String
    public var version: String
    public var title: String?
    public var description: String?
    public var websiteUrl: String?
}

public struct HarnMCPRequestMeta: Codable, Sendable, Equatable {
    public var protocolVersion: String
    public var clientInfo: HarnMCPImplementation
    public var clientCapabilities: HarnACPObject
    public var logLevel: HarnMCPLoggingLevel?
    public var progressToken: HarnACPValue?
    public var traceparent: String?
    public var tracestate: String?
    public var baggage: String?

    enum CodingKeys: String, CodingKey {
        case protocolVersion = "io.modelcontextprotocol/protocolVersion"
        case clientInfo = "io.modelcontextprotocol/clientInfo"
        case clientCapabilities = "io.modelcontextprotocol/clientCapabilities"
        case logLevel = "io.modelcontextprotocol/logLevel"
        case progressToken
        case traceparent
        case tracestate
        case baggage
    }
}

public struct HarnMCPHTTPHeaders: Codable, Sendable, Equatable {
    public var protocolVersion: String
    public var method: String
    public var name: String?

    enum CodingKeys: String, CodingKey {
        case protocolVersion = "MCP-Protocol-Version"
        case method = "Mcp-Method"
        case name = "Mcp-Name"
    }
}

public struct HarnMCPCacheHints: Codable, Sendable, Equatable {
    public var ttlMs: Int
    public var cacheScope: HarnMCPCacheScope
}

public struct HarnMCPDiscoverResult: Codable, Sendable, Equatable {
    public var resultType: HarnMCPResultType
    public var supportedVersions: [String]
    public var capabilities: HarnACPObject
    public var ttlMs: Int
    public var cacheScope: HarnMCPCacheScope
    public var instructions: String?
    public var meta: HarnACPObject?

    enum CodingKeys: String, CodingKey {
        case resultType
        case supportedVersions
        case capabilities
        case ttlMs
        case cacheScope
        case instructions
        case meta = "_meta"
    }
}

public struct HarnMCPInputRequiredResult: Codable, Sendable, Equatable {
    public var resultType: HarnMCPResultType
    public var inputRequests: HarnACPObject?
    public var requestState: String?
    public var meta: HarnACPObject?

    enum CodingKeys: String, CodingKey {
        case resultType
        case inputRequests
        case requestState
        case meta = "_meta"
    }
}

public struct HarnMCPUnsupportedProtocolVersionErrorData: Codable, Sendable, Equatable {
    public var requested: String
    public var supported: [String]
}

public struct HarnMCPUnsupportedProtocolVersionError: Codable, Sendable, Equatable {
    public var jsonrpc: String
    public var id: HarnJsonRpcId?
    public var error: HarnACPError
}

public struct HarnMCPTool: Codable, Sendable, Equatable {
    public var name: String
    public var title: String?
    public var description: String?
    public var inputSchema: HarnMCPJsonSchema202012
    public var outputSchema: HarnMCPJsonSchema202012?
    public var annotations: HarnACPObject?
}

public struct HarnMCPResource: Codable, Sendable, Equatable {
    public var uri: String
    public var name: String
    public var title: String?
    public var description: String?
    public var mimeType: String?
}

public struct HarnMCPResourceTemplate: Codable, Sendable, Equatable {
    public var uriTemplate: String
    public var name: String
    public var title: String?
    public var description: String?
    public var mimeType: String?
}

public struct HarnMCPPrompt: Codable, Sendable, Equatable {
    public var name: String
    public var title: String?
    public var description: String?
    public var arguments: [HarnACPObject]?
}

public struct HarnMCPOAuthProtectedResourceMetadata: Codable, Sendable, Equatable {
    public var resource: String?
    public var authorizationServers: [String]
    public var scopesSupported: [String]?
    public var bearerMethodsSupported: [String]?

    enum CodingKeys: String, CodingKey {
        case resource
        case authorizationServers = "authorization_servers"
        case scopesSupported = "scopes_supported"
        case bearerMethodsSupported = "bearer_methods_supported"
    }
}

public struct HarnMCPOAuthAuthorizationServerMetadata: Codable, Sendable, Equatable {
    public var issuer: String
    public var authorizationEndpoint: String
    public var tokenEndpoint: String
    public var registrationEndpoint: String?
    public var tokenEndpointAuthMethodsSupported: [String]?
    public var codeChallengeMethodsSupported: [String]?
    public var scopesSupported: [String]?
    public var clientIdMetadataDocumentSupported: Bool?
    public var authorizationResponseIssParameterSupported: Bool?

    enum CodingKeys: String, CodingKey {
        case issuer
        case authorizationEndpoint = "authorization_endpoint"
        case tokenEndpoint = "token_endpoint"
        case registrationEndpoint = "registration_endpoint"
        case tokenEndpointAuthMethodsSupported = "token_endpoint_auth_methods_supported"
        case codeChallengeMethodsSupported = "code_challenge_methods_supported"
        case scopesSupported = "scopes_supported"
        case clientIdMetadataDocumentSupported = "client_id_metadata_document_supported"
        case authorizationResponseIssParameterSupported = "authorization_response_iss_parameter_supported"
    }
}

public struct HarnMCPOAuthWwwAuthenticateChallenge: Codable, Sendable, Equatable {
    public var scheme: String
    public var params: [String: String]
}

public struct HarnMCPOAuthDiscoveryResult: Codable, Sendable, Equatable {
    public var protectedResourceMetadataUrl: String
    public var protectedResourceMetadata: HarnMCPOAuthProtectedResourceMetadata
    public var authorizationServerIssuer: String
    public var authorizationServerMetadataUrl: String
    public var authorizationServerMetadataKind: String
    public var authorizationServerMetadata: HarnMCPOAuthAuthorizationServerMetadata
    public var challenge: HarnMCPOAuthWwwAuthenticateChallenge?
    public var scopes: [String]
}

public struct HarnMCPOAuthDynamicClientRegistrationRequest: Codable, Sendable, Equatable {
    public var clientName: String
    public var redirectUris: [String]
    public var grantTypes: [String]
    public var responseTypes: [String]
    public var tokenEndpointAuthMethod: String
    public var applicationType: HarnMCPOAuthApplicationType
    public var scope: String?

    enum CodingKeys: String, CodingKey {
        case clientName = "client_name"
        case redirectUris = "redirect_uris"
        case grantTypes = "grant_types"
        case responseTypes = "response_types"
        case tokenEndpointAuthMethod = "token_endpoint_auth_method"
        case applicationType = "application_type"
        case scope
    }
}
"#,
    );
    append_session_timeline_types(&mut out);
    append_swift_session_update_payloads(&mut out);
    append_swift_prepared_session_types(&mut out);
    append_swift_session_recap_types(&mut out);
    out
}
