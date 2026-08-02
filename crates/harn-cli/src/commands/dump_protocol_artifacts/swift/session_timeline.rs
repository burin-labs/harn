pub(super) fn append_session_timeline_types(out: &mut String) {
    out.push_str(
        r"public struct HarnSessionTimelineCursor: Codable, Sendable, Equatable {
    public var topics: [String: UInt64]
}

public struct HarnSessionTimelineQuery: Codable, Sendable, Equatable {
    public var sessionId: String?
    public var runId: String?
    public var runPath: String?
    public var projectId: String?
    public var fromCursor: HarnSessionTimelineCursor
    public var limit: Int?
}

public struct HarnSessionTimelineReference: Codable, Sendable, Equatable {
    public var kind: String
    public var id: String?
    public var topic: String?
    public var eventId: UInt64?
}

public struct HarnSessionTimelineLink: Codable, Sendable, Equatable {
    public var kind: String
    public var targetId: String?
    public var traceId: String?
    public var spanId: String?
    public var eventId: String?
}

/// Harn-owned semantic chronology row. `kind` remains open for forward compatibility.
public struct HarnSessionTimelineNode: Codable, Sendable, Equatable, Identifiable {
    public var id: String
    public var parentId: String?
    public var children: [String]
    public var category: String
    public var kind: String
    public var name: String
    public var status: String
    public var traceId: String?
    public var spanId: String?
    public var occurredAtMs: Int64?
    public var startMs: UInt64?
    public var durationMs: UInt64?
    public var attributes: HarnACPValue
    public var references: [HarnSessionTimelineReference]
    public var links: [HarnSessionTimelineLink]
    public var order: UInt64
}

public struct HarnSessionTimelineCoverage: Codable, Sendable, Equatable {
    public var returned: Int
    public var available: Int?
    public var truncated: Bool
}

public struct HarnSessionTimelineSnapshot: Codable, Sendable, Equatable {
    public var schemaVersion: UInt32
    public var query: HarnSessionTimelineQuery
    public var cursor: HarnSessionTimelineCursor
    public var coverage: HarnSessionTimelineCoverage
    public var nodes: [HarnSessionTimelineNode]
}

public struct HarnSessionTimelineUpdate: Codable, Sendable, Equatable {
    public var schemaVersion: UInt32
    public var cursor: HarnSessionTimelineCursor
    public var node: HarnSessionTimelineNode
}
",
    );
}
