import Foundation

private enum RecapBindingProbeError: Error {
    case expectedAvailableRecap
    case expectedUnknownSnapshotFieldRejection
    case expectedUnknownVerificationStatusRejection
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
