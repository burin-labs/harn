`DecisionContract` is now defined by `harn-provider-catalog`; its previous `harn-vm`
import path remains a re-export.

Migration: Keep existing imports, or import `DecisionContract` directly from
`harn_provider_catalog`. Regenerate bindings that record the defining crate.
