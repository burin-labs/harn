- **Shared call-cost settlement.** Rust hosts, `std/llm/economics.settle_call_cost`, and the existing
  `estimate_call_cost` helper now price normalized usage through the runtime's settlement owner.
  They apply request-time prices, input bands, one-hour cache writes, hosted tools, and audio rates.
  Partial estimates keep the subtotal and name missing prices; monthly account allowances remain
  explicitly unapplied. Downstream hosts can use this shared pricing boundary.
  Free inference does not hide a hosted-tool fee or a missing hosted price.

  Migration: handle nil `estimate_session_cost(...).total_cost_usd` and per-model `cost_usd` when any
  call is unpriced. Use `known_cost_usd` for the subtotal. Call estimates reject negative or
  contradictory token counts, and their breakdown adds `adjustments_usd` for hosted-tool and audio charges.
