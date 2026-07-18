Upgraded dependencies across the Cargo workspace and the portal package:
`aes-gcm` 0.10 -> 0.11, `jsonschema` 0.46 -> 0.48, `tokio-tungstenite`
0.29 -> 0.30, `tree-sitter-swift` 0.7.2 -> 0.7.3, plus `vite`, `react-intl`,
and `typescript-eslint` in the portal. The OAuth file-storage backend now
builds AES-GCM keys and nonces through `From`/`TryFrom` instead of the
deprecated `Array::from_slice`; the on-disk envelope format is unchanged.
