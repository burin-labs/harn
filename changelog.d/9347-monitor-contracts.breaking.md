Monitor sources and waits retain their state type through `MonitorSource<T>`,
`MonitorWaitOptions<T>`, and `MonitorWaitResult<T>`. GitHub, Linear, and Slack
monitor factories expose named state records. Raw provider values remain
`unknown`, including extra fields and nominal record containers. Slack and
Linear pollers now handle the initial absent push event instead of rejecting it.
Refs #9347.

### Migration

Give custom pollers a state contract when static field checking is needed:

```harn,ignore
import { MonitorSource, wait_for } from "std/monitors"

type ReadyState = {ready: bool}
const source: MonitorSource<ReadyState> = {poll: { _ -> {ready: true} }}
const result = wait_for(harness.runtime, {
  timeout: 1s,
  source: source,
  condition: { state -> state.ready },
})
if result.state != nil {
  harness.stdio.println(result.state.ready)
}
```

Check `result.state` before field access because an interrupted wait may have
no polled state. Decode opaque provider data with `schema_expect` or
`schema_parse` instead of reading fields through an erased dictionary:

```harn,ignore
type MessageFields = {channel?: string, text?: string, ...dict<string, unknown>}
const message = schema_expect(state.message, schema_of(MessageFields))
```

See the [monitor reference](../docs/src/stdlib/monitors.md) for the exported
types and event-envelope decoder.
