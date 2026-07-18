use harn_serve::adapters::acp::{
    ACP_PROMPT_ERROR_DATA_SCHEMA, ACP_SCHEMA_COMPATIBILITY, HARN_AGENT_EVENT_KINDS,
    HARN_AGENT_EVENT_METHOD, HARN_CONTENT_EXTENSION_FIELDS, HARN_PROVIDER_CATALOG_METHOD,
    HARN_SESSION_UPDATE_EXTENSIONS, HARN_TOOL_LIFECYCLE_EXTENSION_FIELDS,
};
use harn_serve::MCP_PROTOCOL_VERSION;
use harn_vm::llm::receipts::{TOOL_CALL_RECEIPT_EXECUTORS, TOOL_CALL_RECEIPT_STATUSES};

use super::constants::*;
use super::support::*;
use super::values::*;

#[cfg(test)]
pub(super) fn generate_swift() -> String {
    generate_swift_for_version(env!("CARGO_PKG_VERSION"))
}

pub(super) fn generate_swift_for_version(artifact_version: &str) -> String {
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
        ("mcpStableProtocolVersion", MCP_PROTOCOL_VERSION),
        ("mcpDraftProtocolVersion", MCP_DRAFT_PROTOCOL_VERSION),
        (
            "mcpLegacy20250618ProtocolVersion",
            MCP_LEGACY_2025_06_18_PROTOCOL_VERSION,
        ),
        (
            "mcpFinal2026ProtocolVersion",
            MCP_FINAL_2026_PROTOCOL_VERSION,
        ),
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
        "toolCallReceiptStatuses",
        TOOL_CALL_RECEIPT_STATUSES,
    ));
    out.push_str(&swift_string_array(
        "toolCallReceiptExecutors",
        TOOL_CALL_RECEIPT_EXECUTORS,
    ));
    out.push_str("}\n\n");

    out.push_str(&swift_enum_with_deprecations(
        "HarnACPAgentMethod",
        &strs_to_strings(ACP_AGENT_METHODS),
        ACP_DEPRECATED_AGENT_METHODS,
    ));
    out.push_str(&swift_enum(
        "HarnACPClientMethod",
        &strs_to_strings(ACP_CLIENT_METHODS),
    ));
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
    out.push_str(&swift_enum("HarnWorkerStatus", &worker_status_values()));
    out.push_str(&swift_enum(
        "HarnAgentTerminalClass",
        &agent_terminal_class_values(),
    ));
    out.push_str(&swift_enum(
        "HarnACPPromptErrorSchema",
        &[ACP_PROMPT_ERROR_DATA_SCHEMA.to_string()],
    ));
    out.push_str(
        r"public struct HarnACPPromptErrorData: Codable, Sendable, Equatable {
    public var schema: HarnACPPromptErrorSchema
    public var terminalClass: HarnAgentTerminalClass
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
    public var argSchema: HarnToolArgSchema
    public var capabilities: [String: [String]]
    public var emitsArtifacts: Bool
    public var resultReaders: [String]
    public var inlineResult: Bool

    enum CodingKeys: String, CodingKey {
        case kind
        case sideEffectLevel = "side_effect_level"
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
    public var serverInfo: HarnMCPImplementation
    public var instructions: String?
    public var meta: HarnACPObject?

    enum CodingKeys: String, CodingKey {
        case resultType
        case supportedVersions
        case capabilities
        case serverInfo
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

    out
}

pub(super) fn swift_enum(name: &str, values: &[String]) -> String {
    swift_enum_with_deprecations(name, values, &[])
}

pub(super) fn swift_enum_with_deprecations(
    name: &str,
    values: &[String],
    deprecated_values: &[DeprecatedWireValue],
) -> String {
    let mut out = format!("public enum {name}: String, Codable, Sendable, CaseIterable {{\n");
    for value in values {
        if let Some(deprecated) = deprecated_wire_value(deprecated_values, value) {
            out.push_str("    @available(*, deprecated, message: ");
            out.push_str(&json_string_literal(&deprecation_message(deprecated)));
            out.push_str(")\n");
        }
        out.push_str("    case ");
        out.push_str(&swift_case_name(value));
        out.push_str(" = ");
        out.push_str(&json_string_literal(value));
        out.push('\n');
    }
    out.push_str("\n    public static let allCases: [Self] = [\n");
    for value in values {
        out.push_str("        ");
        out.push_str(&json_string_literal(value));
        out.push_str(",\n");
    }
    out.push_str("    ].map { Self(rawValue: $0)! }\n");
    out.push_str("}\n\n");
    out
}

pub(super) fn swift_string_array(name: &str, values: &[&str]) -> String {
    let mut out = format!("    public static let {name}: [String] = [\n");
    for value in values {
        out.push_str("        ");
        out.push_str(&json_string_literal(value));
        out.push_str(",\n");
    }
    out.push_str("    ]\n");
    out
}

pub(super) fn swift_case_name(value: &str) -> String {
    let mut out = String::new();
    for (index, part) in value
        .split(['_', '-', '/'])
        .filter(|part| !part.is_empty())
        .enumerate()
    {
        if index == 0 {
            out.push_str(part);
        } else {
            let mut chars = part.chars();
            if let Some(first) = chars.next() {
                out.extend(first.to_uppercase());
                out.push_str(chars.as_str());
            }
        }
    }
    if out.is_empty() {
        "unknown".to_string()
    } else if out.chars().next().is_some_and(|ch| ch.is_ascii_digit()) {
        format!("_{out}")
    } else if SWIFT_RESERVED_KEYWORDS.contains(&out.as_str()) {
        format!("`{out}`")
    } else {
        out
    }
}

/// Swift reserved keywords that cannot appear as bare identifiers in a `case`
/// declaration. Wire values like `private` / `public` (e.g. on
/// `HarnMCPCacheScope`) camelCase down to themselves, so without escaping they
/// land in the generated Swift as `case private = "private"`, which fails to
/// compile.
pub(super) const SWIFT_RESERVED_KEYWORDS: &[&str] = &[
    "associatedtype",
    "break",
    "case",
    "catch",
    "class",
    "continue",
    "default",
    "defer",
    "deinit",
    "do",
    "else",
    "enum",
    "extension",
    "fallthrough",
    "false",
    "fileprivate",
    "for",
    "func",
    "guard",
    "if",
    "import",
    "in",
    "init",
    "inout",
    "internal",
    "is",
    "let",
    "nil",
    "open",
    "operator",
    "private",
    "protocol",
    "public",
    "repeat",
    "return",
    "rethrows",
    "self",
    "Self",
    "static",
    "struct",
    "subscript",
    "super",
    "switch",
    "throw",
    "throws",
    "true",
    "try",
    "typealias",
    "var",
    "where",
    "while",
];

pub(super) fn deprecated_wire_value<'a>(
    deprecated_values: &'a [DeprecatedWireValue],
    value: &str,
) -> Option<&'a DeprecatedWireValue> {
    deprecated_values
        .iter()
        .find(|deprecated| deprecated.value == value)
}

pub(super) fn deprecation_message(value: &DeprecatedWireValue) -> String {
    format!(
        "Use {}; {} will be removed after one release.",
        value.replacement, value.value
    )
}

pub(super) fn wire_value_property_name(value: &str) -> String {
    swift_case_name(value)
}
