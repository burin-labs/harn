import Foundation

@main
struct RunSessionViewBindingProbe {
    static func main() throws {
        let decoder = JSONDecoder()
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

        for malformed in [
            #"{"run":{"started_at":null}}"#,
            #"{"usage":{"input_tokens":"10"}}"#,
            #"{"run":{"duration_ms":-1}}"#,
            #"{"evidence":{"flight_recording":{"retained_events":1}}}"#,
            #"{"metadata":{"execution":{"environment_policy":"unknown"}}}"#,
        ] {
            precondition((try? decoder.decode(HarnRunView.self, from: Data(malformed.utf8))) == nil)
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
