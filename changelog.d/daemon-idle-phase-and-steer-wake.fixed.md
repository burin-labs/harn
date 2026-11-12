An idle daemon agent loop now reports a `turn_phase_changed` event with phase `idle` once per idle
stretch, so a client can show it is waiting rather than working. A steer now wakes an idle daemon on
its own: the idle wait is the end of a step, where a steer is delivered, but the wait took only
`interrupt_immediate` input, so a steer stayed queued until a reminder or other input arrived.
