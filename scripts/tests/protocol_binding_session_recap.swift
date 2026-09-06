import Foundation

private enum RecapBindingProbeError: Error {
    case expectedAvailableRecap
    case expectedUnknownSnapshotFieldRejection
    case expectedUnknownNestedFieldRejection
    case expectedUnknownVerificationStatusRejection
    case wireMismatch
}

private func roundTrip<T: Codable>(_ type: T.Type, _ input: [String: Any], _ expected: [String: Any]) throws {
    let data = try JSONSerialization.data(withJSONObject: input)
    let decoded = try JSONDecoder().decode(type, from: data)
    let encoded = try JSONEncoder().encode(decoded)
    let actual = try JSONSerialization.jsonObject(with: encoded) as! NSDictionary
    guard actual.isEqual(to: expected) else { throw RecapBindingProbeError.wireMismatch }
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
        guard CommandLine.arguments.count == 2 else {
            fatalError("usage: protocol-binding-session-recap FIXTURE")
        }
        let fixture = try JSONSerialization.jsonObject(
            with: Data(contentsOf: URL(fileURLWithPath: CommandLine.arguments[1]))
        ) as! [String: Any]
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
