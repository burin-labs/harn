`harn check` no longer reports an embedder's declared host operations as
unknown capability methods. A host registers these at runtime, so no static
contract in the workspace owns them; `[check].host_capabilities` /
`host_capabilities_path` is the project's declaration that they exist, and the
capability-method check now reads it. Genuine misspellings on VM-contracted
capabilities are still errors.
