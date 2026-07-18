Upgraded the Cargo dependencies whose latest releases sit outside Dependabot's
7-day cooldown: `aes-gcm` 0.10 -> 0.11, `tokio-tungstenite` 0.29 -> 0.30, and
`tree-sitter-swift` 0.7.2 -> 0.7.3. The OAuth file-storage backend now builds
AES-GCM keys and nonces through `From`/`TryFrom` instead of the deprecated
`Array::from_slice`; the on-disk envelope format is unchanged.
