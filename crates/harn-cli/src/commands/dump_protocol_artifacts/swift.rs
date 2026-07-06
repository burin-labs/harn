use harn_serve::adapters::acp::{
    ACP_SCHEMA_COMPATIBILITY, HARN_AGENT_EVENT_KINDS, HARN_AGENT_EVENT_METHOD,
    HARN_CONTENT_EXTENSION_FIELDS, HARN_PROVIDER_CATALOG_METHOD, HARN_SESSION_UPDATE_EXTENSIONS,
    HARN_TOOL_LIFECYCLE_EXTENSION_FIELDS,
};
use harn_serve::MCP_PROTOCOL_VERSION;
use harn_vm::llm::receipts::{TOOL_CALL_RECEIPT_EXECUTORS, TOOL_CALL_RECEIPT_STATUSES};

use super::constants::*;
use super::support::*;
use super::values::*;

pub(super) fn generate_swift() -> String {
    let mut out = generated_header("harn dump-protocol-artifacts", "swift");
    out.push_str("import Foundation\n\n");
    out.push_str("public enum HarnProtocolConstants {\n");
    out.push_str(&format!(
        "    public static const artifactVersion = {}\n",
        json_string_literal(env!("CARGO_PKG_VERSION"))
    ));
    out.push_str(&format!(
        "    public static const acpSchemaCompatibility = {}\n",
        json_string_literal(ACP_SCHEMA_COMPATIBILITY)
    ));
    out.push_str(&format!(
        "    public static const harnAgentEventMethod = {}\n",
        json_string_literal(HARN_AGENT_EVENT_METHOD)
    ));
    out.push_str(&format!(
        "    public static const harnProviderCatalogMethod = {}\n",
        json_string_literal(HARN_PROVIDER_CATALOG_METHOD)
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
            "    public static const {name} = {}\n",
            json_string_literal(value)
        ));
    }
    out.push_str(&format!(
        "    public static const mcpUnsupportedProtocolVersionErrorCode = {MCP_UNSUPPORTED_PROTOCOL_VERSION_ERROR_CODE}\n"
    ));
    out.push_str(&format!(
        "    public static const mcpUnsupportedProtocolVersionErrorMessage = {}\n",
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
        "HarnSideEffectLevel",
        &side_effect_level_values(),
    ));
    out.push_str(&swift_enum("HarnWorkerStatus", &worker_status_values()));
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
        const encoder = JSONEncoder()
        guard const data = try? encoder.encode(HarnAnyEncodable(value)),
              const object = try? JSONSerialization.jsonObject(with: data),
              const converted = HarnACPValue(jsonObject: object) else {
            return nil
        }
        self = converted
    }

    public init?(jsonObject: Any) {
        if const scalar = Self.jsonScalar(jsonObject) {
            self = scalar
        } else if const values = jsonObject as? [Any] {
            guard const array = Self.jsonArray(values) else { return nil }
            self = array
        } else if const values = jsonObject as? [String: Any] {
            guard const object = Self.jsonDictionary(values) else { return nil }
            self = object
        } else {
            return nil
        }
    }

    public init(from decoder: Decoder) throws {
        const container = try decoder.singleValueContainer()
        if container.decodeNil() {
            self = .null
        } else if const value = try? container.decode(Bool.self) {
            self = .bool(value)
        } else if const value = try? container.decode(Int.self) {
            self = .int(value)
        } else if const value = try? container.decode(Double.self) {
            self = .double(value)
        } else if const value = try? container.decode(String.self) {
            self = .string(value)
        } else if const value = try? container.decode([HarnACPValue].self) {
            self = .array(value)
        } else if const value = try? container.decode([String: HarnACPValue].self) {
            self = .object(value)
        } else {
            self = .null
        }
    }

    public func encode(to encoder: Encoder) throws {
        let container = encoder.singleValueContainer()
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

    public let displayString: String {
        switch self {
        case .null: return "nil"
        case .bool(let value): return value ? "true" : "false"
        case .int(let value): return "\(value)"
        case .double(let value): return "\(value)"
        case .string(let value): return value
        case .array(let value): return "[\(value.map(\.displayString).joined(separator: ", "))]"
        case .object(let value):
            const pairs = value.sorted(by: { $0.key < $1.key })
                .map { "\($0.key): \($0.value.displayString)" }
            return "{\(pairs.joined(separator: ", "))}"
        }
    }

    public let stringValue: String? {
        if case .string(let value) = self { return value }
        return nil
    }

    public let intValue: Int? {
        if case .int(let value) = self { return value }
        if case .double(let value) = self, value.rounded() == value { return Int(value) }
        return nil
    }

    public let boolValue: Bool? {
        if case .bool(let value) = self { return value }
        return nil
    }

    public let arrayValue: [HarnACPValue]? {
        if case .array(let value) = self { return value }
        return nil
    }

    public let objectValue: [String: HarnACPValue]? {
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
        const objCType = String(cString: value.objCType)
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
        let items: [HarnACPValue] = []
        items.reserveCapacity(values.count)
        for value in values {
            guard const item = HarnACPValue(jsonObject: value) else { return nil }
            items.append(item)
        }
        return .array(items)
    }

    private static func jsonDictionary(_ values: [String: Any]) -> HarnACPValue? {
        let fields: [String: HarnACPValue] = [:]
        fields.reserveCapacity(values.count)
        for (key, value) in values {
            guard const item = HarnACPValue(jsonObject: value) else { return nil }
            fields[key] = item
        }
        return .object(fields)
    }
}

public typealias HarnACPObject = [String: HarnACPValue]

private struct HarnAnyEncodable: Encodable {
    const value: Encodable

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
        const container = try decoder.singleValueContainer()
        if container.decodeNil() {
            self = .null
        } else if const value = try? container.decode(Int.self) {
            self = .int(value)
        } else if const value = try? container.decode(String.self) {
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
        let container = encoder.singleValueContainer()
        switch self {
        case .null: try container.encodeNil()
        case .int(let value): try container.encode(value)
        case .string(let value): try container.encode(value)
        }
    }

    public let intValue: Int? {
        if case .int(let value) = self { return value }
        return nil
    }

    public let stringValue: String? {
        if case .string(let value) = self { return value }
        return nil
    }
}

public struct HarnACPRequest: Codable, Sendable, Equatable {
    public const jsonrpc: String
    public let id: HarnJsonRpcId
    public let method: String
    public let params: HarnACPValue?

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
    public let code: Int
    public let message: String
    public let data: HarnACPValue?

    public init(code: Int, message: String, data: HarnACPValue? = nil) {
        self.code = code
        self.message = message
        self.data = data
    }
}

public struct HarnACPResponse: Codable, Sendable, Equatable {
    public const jsonrpc: String
    public let id: HarnJsonRpcId
    public let result: HarnACPValue?
    public let error: HarnACPError?

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
    public const jsonrpc: String
    public let method: String
    public let params: HarnACPValue?

    public init(method: String, params: HarnACPValue? = nil) {
        self.jsonrpc = "2.0"
        self.method = method
        self.params = params
    }
}

public struct HarnACPExtensionMeta: Codable, Sendable, Equatable {
    public let harn: HarnACPObject?
}

public struct HarnACPContentBlock: Codable, Sendable, Equatable {
    public let type: String
    public let text: String?
    public let meta: HarnACPExtensionMeta?

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
        if const raw = try? decoder.singleValueContainer().decode(String.self) {
            switch raw {
            case "harn_builtin": self = .harnBuiltin
            case "host_bridge": self = .hostBridge
            case "provider_native": self = .providerNative
            default: self = .unknown(raw)
            }
            return
        }
        const object = try decoder.container(keyedBy: ObjectKey.self)
        const kind = try object.decode(String.self, forKey: .kind)
        if kind == "mcp_server" {
            self = .mcpServer(name: try object.decode(String.self, forKey: .serverName))
        } else {
            self = .unknown(kind)
        }
    }

    public func encode(to encoder: Encoder) throws {
        switch self {
        case .harnBuiltin:
            let container = encoder.singleValueContainer()
            try container.encode("harn_builtin")
        case .hostBridge:
            let container = encoder.singleValueContainer()
            try container.encode("host_bridge")
        case .providerNative:
            let container = encoder.singleValueContainer()
            try container.encode("provider_native")
        case .mcpServer(let name):
            let container = encoder.container(keyedBy: ObjectKey.self)
            try container.encode("mcp_server", forKey: .kind)
            try container.encode(name, forKey: .serverName)
        case .unknown(let raw):
            let container = encoder.singleValueContainer()
            try container.encode(raw)
        }
    }

    public let displayLabel: String {
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
    public let audit: HarnACPValue?
    public let durationMs: Double?
    public let error: String?
    public let errorCategory: HarnToolCallErrorCategory?
    public let executionDurationMs: Double?
    public let executor: HarnACPToolExecutor?
    public let parsing: Bool?
    public let rawInputPartial: String?
}

public struct HarnToolCallReceipt: Codable, Sendable, Equatable {
    public let schemaVersion: Int
    public let sessionId: String
    public let runId: String?
    public let toolCallId: String
    public let toolName: String
    public let iteration: Int
    public let turnIndex: Int?
    public let emitOrder: Int
    public let reason: String?
    public let kind: String?
    public let executor: HarnToolCallReceiptExecutor?
    public let status: HarnToolCallReceiptStatus
    public let errorCategory: String?
    public let durationMs: Int
    public let argsHash: String
    public let resultHash: String?
    public let audit: HarnACPValue
    public let emittedAt: String
    public let model: String?
    public let provider: String?

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
    public let sessionUpdate: HarnACPSessionUpdate
    public let toolCallId: String
    public let title: String
    public let kind: HarnACPToolKind?
    public let status: HarnACPToolCallStatus?
    public let content: [HarnACPContentBlock]?
    public let locations: [HarnACPValue]?
    public let rawInput: HarnACPValue?
    public let rawOutput: HarnACPValue?
    public let meta: HarnACPExtensionMeta?

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
    public let sessionUpdate: HarnACPSessionUpdate
    public let content: HarnACPValue?
    public let messageId: String?
    public let entries: [HarnACPValue]?
    public let keptTurnCount: Int?
    public let removedTurnCount: Int?
    public let newTipTurnId: String?
    public let reason: String?
    public let toolCallId: String?
    public let title: String?
    public let kind: HarnACPToolKind?
    public let status: HarnACPToolCallStatus?
    public let rawInput: HarnACPValue?
    public let rawOutput: HarnACPValue?
    public let meta: HarnACPExtensionMeta?

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
    public let sessionId: String
    public let update: HarnACPSessionUpdateEnvelope
}

public struct HarnACPSessionUpdateNotification: Codable, Sendable, Equatable {
    public const jsonrpc: String
    public let method: String
    public let params: HarnACPSessionUpdateParams
}

public struct HarnAgentEventNotification: Codable, Sendable, Equatable {
    public const jsonrpc: String
    public let method: String
    public let params: HarnACPObject
}

public struct HarnPromptCapabilities: Codable, Sendable, Equatable {
    public let image: Bool?
    public let audio: Bool?
    public let embeddedContext: Bool?
}

public struct HarnACPAgentCapabilities: Codable, Sendable, Equatable {
    public let meta: HarnACPExtensionMeta?
    public let loadSession: Bool?
    public let session: HarnACPObject?
    public let promptCapabilities: HarnPromptCapabilities?
    public let mcpCapabilities: HarnACPObject?
    public let sessionCapabilities: HarnACPObject?

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
    public let pathParams: [String]
    public let argAliases: [String: String]
    public let required: [String]

    enum CodingKeys: String, CodingKey {
        case pathParams = "path_params"
        case argAliases = "arg_aliases"
        case required
    }
}

public struct HarnToolAnnotations: Codable, Sendable, Equatable {
    public let kind: HarnACPToolKind
    public let sideEffectLevel: HarnSideEffectLevel
    public let argSchema: HarnToolArgSchema
    public let capabilities: [String: [String]]
    public let emitsArtifacts: Bool
    public let resultReaders: [String]
    public let inlineResult: Bool

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
    public let state: HarnA2ATaskState
    public let message: HarnACPValue?
    public let timestamp: String?
}

public struct HarnA2ATask: Codable, Sendable, Equatable {
    public let id: String
    public let contextId: String?
    public let status: HarnA2ATaskStatus
    public let history: [HarnACPValue]?
    public let artifacts: [HarnACPValue]?
    public let metadata: HarnACPObject?
}

public typealias HarnMCPJsonSchema202012 = HarnACPObject

public struct HarnMCPImplementation: Codable, Sendable, Equatable {
    public let name: String
    public let version: String
    public let title: String?
    public let description: String?
    public let websiteUrl: String?
}

public struct HarnMCPRequestMeta: Codable, Sendable, Equatable {
    public let protocolVersion: String
    public let clientInfo: HarnMCPImplementation
    public let clientCapabilities: HarnACPObject
    public let logLevel: HarnMCPLoggingLevel?
    public let progressToken: HarnACPValue?
    public let traceparent: String?
    public let tracestate: String?
    public let baggage: String?

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
    public let protocolVersion: String
    public let method: String
    public let name: String?

    enum CodingKeys: String, CodingKey {
        case protocolVersion = "MCP-Protocol-Version"
        case method = "Mcp-Method"
        case name = "Mcp-Name"
    }
}

public struct HarnMCPCacheHints: Codable, Sendable, Equatable {
    public let ttlMs: Int
    public let cacheScope: HarnMCPCacheScope
}

public struct HarnMCPDiscoverResult: Codable, Sendable, Equatable {
    public let resultType: HarnMCPResultType
    public let supportedVersions: [String]
    public let capabilities: HarnACPObject
    public let serverInfo: HarnMCPImplementation
    public let instructions: String?
    public let meta: HarnACPObject?

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
    public let resultType: HarnMCPResultType
    public let inputRequests: HarnACPObject?
    public let requestState: String?
    public let meta: HarnACPObject?

    enum CodingKeys: String, CodingKey {
        case resultType
        case inputRequests
        case requestState
        case meta = "_meta"
    }
}

public struct HarnMCPUnsupportedProtocolVersionErrorData: Codable, Sendable, Equatable {
    public let requested: String
    public let supported: [String]
}

public struct HarnMCPUnsupportedProtocolVersionError: Codable, Sendable, Equatable {
    public let jsonrpc: String
    public let id: HarnJsonRpcId?
    public let error: HarnACPError
}

public struct HarnMCPTool: Codable, Sendable, Equatable {
    public let name: String
    public let title: String?
    public let description: String?
    public let inputSchema: HarnMCPJsonSchema202012
    public let outputSchema: HarnMCPJsonSchema202012?
    public let annotations: HarnACPObject?
}

public struct HarnMCPResource: Codable, Sendable, Equatable {
    public let uri: String
    public let name: String
    public let title: String?
    public let description: String?
    public let mimeType: String?
}

public struct HarnMCPResourceTemplate: Codable, Sendable, Equatable {
    public let uriTemplate: String
    public let name: String
    public let title: String?
    public let description: String?
    public let mimeType: String?
}

public struct HarnMCPPrompt: Codable, Sendable, Equatable {
    public let name: String
    public let title: String?
    public let description: String?
    public let arguments: [HarnACPObject]?
}

public struct HarnMCPOAuthProtectedResourceMetadata: Codable, Sendable, Equatable {
    public let resource: String?
    public let authorizationServers: [String]
    public let scopesSupported: [String]?
    public let bearerMethodsSupported: [String]?

    enum CodingKeys: String, CodingKey {
        case resource
        case authorizationServers = "authorization_servers"
        case scopesSupported = "scopes_supported"
        case bearerMethodsSupported = "bearer_methods_supported"
    }
}

public struct HarnMCPOAuthAuthorizationServerMetadata: Codable, Sendable, Equatable {
    public let issuer: String
    public let authorizationEndpoint: String
    public let tokenEndpoint: String
    public let registrationEndpoint: String?
    public let tokenEndpointAuthMethodsSupported: [String]?
    public let codeChallengeMethodsSupported: [String]?
    public let scopesSupported: [String]?
    public let clientIdMetadataDocumentSupported: Bool?
    public let authorizationResponseIssParameterSupported: Bool?

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
    public let scheme: String
    public let params: [String: String]
}

public struct HarnMCPOAuthDiscoveryResult: Codable, Sendable, Equatable {
    public let protectedResourceMetadataUrl: String
    public let protectedResourceMetadata: HarnMCPOAuthProtectedResourceMetadata
    public let authorizationServerIssuer: String
    public let authorizationServerMetadataUrl: String
    public let authorizationServerMetadataKind: String
    public let authorizationServerMetadata: HarnMCPOAuthAuthorizationServerMetadata
    public let challenge: HarnMCPOAuthWwwAuthenticateChallenge?
    public let scopes: [String]
}

public struct HarnMCPOAuthDynamicClientRegistrationRequest: Codable, Sendable, Equatable {
    public let clientName: String
    public let redirectUris: [String]
    public let grantTypes: [String]
    public let responseTypes: [String]
    public let tokenEndpointAuthMethod: String
    public let applicationType: HarnMCPOAuthApplicationType
    public let scope: String?

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
    out.push_str("\n    public static const allCases: [Self] = [\n");
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
    let mut out = format!("    public static const {name}: [String] = [\n");
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
