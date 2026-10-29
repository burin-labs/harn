import Foundation

private enum RecapBindingProbeError: Error {
    case expectedAvailableRecap
    case expectedUnknownSnapshotFieldRejection
    case expectedUnknownNestedFieldRejection
    case expectedUnknownVerificationStatusRejection
    case wireMismatch(type: String, expected: String, actual: String)
    case incompleteSessionUpdateFixture
    case acceptedMissingSessionUpdateIdentity
    case agentEventDecodedAsUntyped(String)
    case acceptedMalformedAgentEvent(String)
}

/// Every emitted agent event decodes into its generated body and re-encodes
/// to the same params; a missing typed field or an unknown kind is refused.
private func checkAgentEvents(_ path: String) throws {
    let events = try JSONSerialization.jsonObject(
        with: Data(contentsOf: URL(fileURLWithPath: path))
    ) as! [[String: Any]]
    for params in events {
        try roundTrip(HarnAgentEvent.self, params, params)
        let decoded = try JSONDecoder().decode(
            HarnAgentEvent.self, from: JSONSerialization.data(withJSONObject: params))
        let body = Mirror(reflecting: decoded.payload).children.first?.value
        let openKinds: Set<HarnAgentEventKind> = [.sessionHealth, .reservedTerminalVerify, .agentLoopStallWarning]
        if body is [String: HarnACPValue], !openKinds.contains(decoded.kind) {
            throw RecapBindingProbeError.agentEventDecodedAsUntyped(decoded.kind.rawValue)
        }
    }
    let judge = events.first { $0["kind"] as? String == "judge_decision" }!
    for invalid in [
        judge.filter { $0.key != "verdict" },
        judge.merging(["kind": "future_event_kind"]) { $1 },
    ] {
        do {
            _ = try JSONDecoder().decode(
                HarnAgentEvent.self, from: JSONSerialization.data(withJSONObject: invalid))
        } catch {
            continue
        }
        throw RecapBindingProbeError.acceptedMalformedAgentEvent(String(describing: invalid))
    }
}

private func roundTrip<T: Codable>(_ type: T.Type, _ input: [String: Any], _ expected: [String: Any]) throws {
    let data = try JSONSerialization.data(withJSONObject: input)
    let decoded = try JSONDecoder().decode(type, from: data)
    let encoded = try JSONEncoder().encode(decoded)
    let actual = try JSONSerialization.jsonObject(with: encoded) as! NSDictionary
    guard actual.isEqual(to: expected) else {
        throw RecapBindingProbeError.wireMismatch(
            type: String(describing: type), expected: String(describing: expected),
            actual: String(describing: actual)
        )
    }
}

private func optionalField<T: Codable>(_ type: T.Type, _ base: [String: Any], _ field: String, _ present: Any) throws {
    try roundTrip(type, base, base)
    var input = base
    input[field] = NSNull()
    try roundTrip(type, input, base)
    input[field] = present
    try roundTrip(type, input, input)
}

@main
private struct RecapBindingProbe {
    static func main() throws {
        guard CommandLine.arguments.count == 5 else {
            fatalError("usage: protocol-binding-session-recap FIXTURE SESSION_UPDATES SESSION_UPDATE_SCHEMA AGENT_EVENTS")
        }
        try checkAgentEvents(CommandLine.arguments[4])
        let fixture = try JSONSerialization.jsonObject(
            with: Data(contentsOf: URL(fileURLWithPath: CommandLine.arguments[1]))
        ) as! [String: Any]
        let notifications = try JSONSerialization.jsonObject(
            with: Data(contentsOf: URL(fileURLWithPath: CommandLine.arguments[2]))
        ) as! [[String: Any]]
        var updateKinds = Set<String>()
        for notification in notifications {
            let params = notification["params"] as! [String: Any]
            var update = params["update"] as! [String: Any]
            updateKinds.insert(update["sessionUpdate"] as! String)
            try roundTrip(HarnACPTypedSessionUpdate.self, update, update)
            var meta = update["_meta"] as! [String: Any]
            var harn = meta["harn"] as! [String: Any]
            harn["replayed"] = true
            switch update["sessionUpdate"] as! String {
            case "artifact": harn["title"] = NSNull()
            case "transcript_compacted":
                harn["snapshotAssetId"] = NSNull()
                harn["compactionPolicy"] = NSNull()
            case "worker_update":
                harn["metadata"] = NSNull()
                harn["audit"] = NSNull()
            case "reminder_emitted":
                var reminder = harn["reminder"] as! [String: Any]
                reminder["ttlTurns"] = NSNull()
                harn["reminder"] = reminder
            default: break
            }
            meta["harn"] = harn
            update["_meta"] = meta
            try roundTrip(HarnACPTypedSessionUpdate.self, update, update)
            update["_meta"] = ["harn": [String: Any]()]
            let invalid = try JSONSerialization.data(withJSONObject: update)
            do {
                _ = try JSONDecoder().decode(HarnACPTypedSessionUpdate.self, from: invalid)
                throw RecapBindingProbeError.acceptedMissingSessionUpdateIdentity
            } catch RecapBindingProbeError.acceptedMissingSessionUpdateIdentity {
                throw RecapBindingProbeError.acceptedMissingSessionUpdateIdentity
            } catch {}
        }
        // Kinds the adapter emits outside AgentEvent are absent from the event
        // fixture, so the schema carries an example payload for each. A kind
        // with neither source fails the coverage check below.
        let schema = try JSONSerialization.jsonObject(
            with: Data(contentsOf: URL(fileURLWithPath: CommandLine.arguments[3]))
        ) as! [String: Any]
        let definitions = schema["$defs"] as! [String: Any]
        let variants = (definitions["HarnExtensionUpdate"] as! [String: Any])["oneOf"] as! [[String: Any]]
        var examples: [[String: Any]] = []
        var schemaKinds = Set<String>()
        for variant in variants {
            let key = String((variant["$ref"] as! String).dropFirst("#/$defs/".count))
            let definition = definitions[key] as! [String: Any]
            let properties = definition["properties"] as! [String: Any]
            schemaKinds.insert((properties["sessionUpdate"] as! [String: Any])["const"] as! String)
            examples += (definition["examples"] as? [[String: Any]]) ?? []
        }
        for example in examples { try roundTrip(HarnACPTypedSessionUpdate.self, example, example) }
        let nonEventKinds = Set(examples.map { $0["sessionUpdate"] as! String })
        guard schemaKinds == Set(HarnProtocolConstants.acpSessionUpdateExtensions),
              updateKinds.union(nonEventKinds) == schemaKinds,
              updateKinds.isDisjoint(with: nonEventKinds) else {
            throw RecapBindingProbeError.incompleteSessionUpdateFixture
        }
        let recap = fixture["sessionRecapAvailability"] as! [String: Any]
        let plan = fixture["planDocument"] as! [String: Any]
        try roundTrip(HarnPlanDocument.self, plan, plan)
        try optionalField(HarnPlanStep.self, ["id": "step", "content": "Verify", "status": "pending"], "priority", "high")
        try optionalField(HarnPlanApproval.self, ["state": "unrequested"], "reviewers", ["reviewer"])
        try optionalField(HarnPlanCommentAnchor.self, ["step_id": "step"], "range", ["start": 0, "end": 1])
        let recapData = try JSONSerialization.data(withJSONObject: recap)
        let decoded = try JSONDecoder().decode(HarnSessionRecapAvailability.self, from: recapData)
        guard decoded.state == .available, decoded.snapshot?.turns.count == 1 else {
            throw RecapBindingProbeError.expectedAvailableRecap
        }

        var unknown = recap
        var snapshot = unknown["snapshot"] as! [String: Any]
        snapshot["futureTopLevel"] = true
        unknown["snapshot"] = snapshot
        let unknownData = try JSONSerialization.data(withJSONObject: unknown)
        do {
            _ = try JSONDecoder().decode(HarnSessionRecapAvailability.self, from: unknownData)
            throw RecapBindingProbeError.expectedUnknownSnapshotFieldRejection
        } catch RecapBindingProbeError.expectedUnknownSnapshotFieldRejection {
            throw RecapBindingProbeError.expectedUnknownSnapshotFieldRejection
        } catch {}

        snapshot.removeValue(forKey: "futureTopLevel")
        var turns = snapshot["turns"] as! [[String: Any]]
        var iterations = turns[0]["iterations"] as! [[String: Any]]
        var tools = iterations[0]["tools"] as! [[String: Any]]
        var verification = tools[0]["verification"] as! [String: Any]
        verification["futureNested"] = true
        tools[0]["verification"] = verification
        iterations[0]["tools"] = tools
        turns[0]["iterations"] = iterations
        snapshot["turns"] = turns
        unknown["snapshot"] = snapshot
        let unknownNestedData = try JSONSerialization.data(withJSONObject: unknown)
        do {
            _ = try JSONDecoder().decode(HarnSessionRecapAvailability.self, from: unknownNestedData)
            throw RecapBindingProbeError.expectedUnknownNestedFieldRejection
        } catch RecapBindingProbeError.expectedUnknownNestedFieldRejection {
            throw RecapBindingProbeError.expectedUnknownNestedFieldRejection
        } catch {}

        verification.removeValue(forKey: "futureNested")
        verification["status"] = "future_status"
        tools[0]["verification"] = verification
        iterations[0]["tools"] = tools
        turns[0]["iterations"] = iterations
        snapshot["turns"] = turns
        unknown["snapshot"] = snapshot
        let invalidStatusData = try JSONSerialization.data(withJSONObject: unknown)
        do {
            _ = try JSONDecoder().decode(HarnSessionRecapAvailability.self, from: invalidStatusData)
            throw RecapBindingProbeError.expectedUnknownVerificationStatusRejection
        } catch RecapBindingProbeError.expectedUnknownVerificationStatusRejection {
            throw RecapBindingProbeError.expectedUnknownVerificationStatusRejection
        } catch {}
    }
}
