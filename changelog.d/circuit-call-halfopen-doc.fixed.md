- Correct the `std/async` `circuit_call` doc comment: it claimed the
  half-open state "allows one call through as a probe", but the underlying
  named-circuit primitive is a pure check-then-act API with no probe
  reservation, so after the cooldown it admits every caller as a probe rather
  than gating to a single concurrent one. The comment now says so, removing a
  latent trap for callers that might have relied on single-probe exclusivity.
