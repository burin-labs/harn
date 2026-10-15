`DecisionContract` is now defined by `harn-provider-catalog`; its previous `harn-vm`
import path remains a re-export. Rust code that depends on the defining crate's type identity
must use the catalog crate.
