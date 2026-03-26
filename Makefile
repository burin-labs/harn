.PHONY: check fmt lint test conformance all

# Full quality check: format, lint, test, conformance
all: fmt lint test conformance

# Format all code
fmt:
	cargo fmt --all

# Run clippy lints (deny warnings in CI)
lint:
	cargo clippy --workspace -- -D warnings

# Run Rust unit tests
test:
	cargo test --workspace

# Run Harn conformance test suite
conformance:
	cargo run --bin harn -- test conformance

# Format check (no changes, for CI)
fmt-check:
	cargo fmt --all -- --check
