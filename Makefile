.PHONY: check fmt lint lint-md test conformance all

# Full quality check: format, lint, test, conformance
all: fmt lint lint-md test conformance

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

# Lint markdown files
lint-md:
	npx markdownlint-cli2 "**/*.md"

# Format check (no changes, for CI)
fmt-check:
	cargo fmt --all -- --check
