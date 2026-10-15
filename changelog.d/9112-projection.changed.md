Provider catalog projection, validation, schemas, bindings, decision contracts, and pure taxonomy
now compile in the catalog crate. Runtime adapters capture configuration and measured capability
receipts once before projection. Redundant validation tests were retired and owning tests moved
with their implementation so the native VM test unit carries less work.
