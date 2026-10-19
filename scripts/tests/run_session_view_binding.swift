import Foundation
import HarnViewProjection

@main
struct RunSessionViewBindingProbe {
    static func main() throws {
        let decoder = JSONDecoder()
        let constructedRun = HarnRunView(schema: harnRunViewV1Schema, schemaVersion: harnRunViewV1SchemaVersion)
        let constructedSession = HarnSessionView(schema: harnSessionViewV1Schema, schemaVersion: harnSessionViewV1SchemaVersion)
        precondition(constructedRun.producer.version.isEmpty && constructedRun.evidence.reasoningReceipts == nil)
        precondition(constructedSession.runs.isEmpty)
        let roundTripRun = try decoder.decode(HarnRunView.self, from: JSONEncoder().encode(constructedRun))
        let roundTripSession = try decoder.decode(HarnSessionView.self, from: JSONEncoder().encode(constructedSession))
        precondition(roundTripRun == constructedRun && roundTripSession == constructedSession)
        let grant = HarnGrantReceipt(name: "provider", sourceKind: "env")
        precondition(grant.exposedAsEnv == nil && grant.exposedTo == .session)
        let recording = HarnFlightRecordingArtifact(
            schemaVersion: 1, executionId: "execution", format: "json", contentHash: "digest",
            byteLength: 12, retainedEvents: 1, droppedEvents: 0, valuePolicy: .omitted
        )
        precondition(recording.path == nil && recording.retainedEvents == 1)
        let historical = try decoder.decode(HarnRunView.self, from: Data(#"{"schema":"harn.run_view.v1","schema_version":1,"producer":{"name":"historical"}}"#.utf8))
        precondition(historical.producer.name == "historical")
        precondition(historical.producer.version.isEmpty)
        precondition(historical.evidence.reasoningReceipts == nil)
        precondition(historical.evidence.evaluationReceipts == nil)

        let measured = try decoder.decode(HarnRunView.self, from: Data(#"{"schema":"harn.run_view.v1","schema_version":1,"evidence":{"reasoning_receipts":[],"evaluation_receipts":[],"trace_spans":[{"span_id":18446744073709551615}]},"metadata":{"execution":{"environment_policy":"granted","grants":[{"name":"provider","source_kind":"env"}]}}}"#.utf8))
        precondition(measured.producer.name.isEmpty && measured.producer.version.isEmpty)
        precondition(measured.evidence.reasoningReceipts?.isEmpty == true)
        precondition(measured.evidence.evaluationReceipts?.isEmpty == true)
        precondition(measured.evidence.traceSpans.first?.spanId == UInt64.max)
        precondition(measured.evidence.traceSpans.first?.events.isEmpty == true)
        precondition(measured.metadata.execution?.grants.first?.exposedTo == .session)

        for malformedIdentity in [
            #"{}"#,
            #"{"schema_version":1}"#,
            #"{"schema":"harn.run_view.v1"}"#,
            #"{"schema":null,"schema_version":1}"#,
            #"{"schema":"harn.run_view.v1","schema_version":null}"#,
        ] {
            let data = Data(malformedIdentity.utf8)
            precondition((try? decoder.decode(HarnRunView.self, from: data)) == nil)
            precondition((try? decoder.decode(HarnSessionView.self, from: data)) == nil)
        }

        for malformed in [
            #"{"run":{"started_at":null}}"#,
            #"{"usage":{"input_tokens":"10"}}"#,
            #"{"run":{"duration_ms":-1}}"#,
            #"{"evidence":{"flight_recording":{"retained_events":1}}}"#,
            #"{"metadata":{"execution":{"environment_policy":"unknown"}}}"#,
        ] {
            let data = Data((#"{"schema":"harn.run_view.v1","schema_version":1,"# + malformed.dropFirst()).utf8)
            precondition((try? decoder.decode(HarnRunView.self, from: data)) == nil)
        }
        for path in CommandLine.arguments.dropFirst() {
            let data = try Data(contentsOf: URL(fileURLWithPath: path))
            if path.hasSuffix("session_view.json") {
                let view = try decoder.decode(HarnSessionView.self, from: data)
                precondition(view.schema == harnSessionViewV1Schema)
                precondition(view.schemaVersion == harnSessionViewV1SchemaVersion)
                precondition(!view.runs.isEmpty)
            } else {
                let view = try decoder.decode(HarnRunView.self, from: data)
                precondition(view.schema == harnRunViewV1Schema)
                precondition(view.schemaVersion == harnRunViewV1SchemaVersion)
                precondition(!view.run.runId.isEmpty)
            }
        }
        print("Native run/session view binding preserves defaults, receipts, unsigned values, and scalar refusals.")
    }
}
