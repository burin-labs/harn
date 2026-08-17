//! EC P-256 key pair used only by the `jwt_sign`/`jwt_verify` round-trip tests.
//!
//! Generated with `openssl genpkey` on 2026-08-16 purely as a test prop. It has
//! never signed for a real service or account and is safe to be public, so a
//! secret scanner reporting it is reporting a false positive. Keeping the pair
//! in its own file gives that allowlist decision one place to point at, and
//! keeps the key material out of `crypto.rs`, which sits under a file-length
//! ratchet that only moves down.
//!
//! The two constants are each other's pair. Regenerate them together or the
//! round-trip tests fail closed, which is the intended signal.

pub(super) const ES256_PRIVATE_KEY: &str = "-----BEGIN PRIVATE KEY-----\n\
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgL/z9S+B6gXXkFHlu\n\
NT/OiT2akIQzHq60993wCey95vGhRANCAATIs1NFRUVKlbbZYE4klZUg82yJrkhW\n\
XAEZEyRQ8LkuxeTs1z7z9FAMXaitN4KB9YSk6ShJpRKTKwwQXRmktGzi\n\
-----END PRIVATE KEY-----\n";

pub(super) const ES256_PUBLIC_KEY: &str = "-----BEGIN PUBLIC KEY-----\n\
MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEyLNTRUVFSpW22WBOJJWVIPNsia5I\n\
VlwBGRMkUPC5LsXk7Nc+8/RQDF2orTeCgfWEpOkoSaUSkysMEF0ZpLRs4g==\n\
-----END PUBLIC KEY-----\n";
