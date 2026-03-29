# Changelog

All notable changes to Harn are documented in this file.

## v0.4.13

### Added

- **Cost tracking**: `llm_cost()`, `llm_session_cost()`, `llm_budget()`,
  `llm_budget_remaining()` builtins for estimating and capping LLM spend
- **Mock HTTP framework**: `http_mock()`, `http_mock_clear()`,
  `http_mock_calls()` for testing pipelines without real HTTP calls
- **Inline evaluation**: `harn run -e 'expression'` for quick one-liners
- **Spread in method calls**: `obj.method(...args)` now works (previously
  only worked in function calls)
- **Provider fallback chains**: `fallback` field in providers.toml for
  automatic retry with a different provider on failure
- **Graceful cancellation**: `cancel_graceful()` and `is_cancelled()`
  builtins for cooperative task shutdown
- Conformance tests for all new features

### Fixed

- Interface satisfaction checking now validates parameter types and return
  types, not just method names and parameter counts
- Generic constraints (`where T: Interface`) now work inside container
  types like `list<T>` and `dict<string, T>`
- Static analysis warns on calls to undefined functions (when receiver
  type is known)

### Changed

- **Code quality**: Split 4 oversized files into focused modules:
  - `stdlib.rs` (3109 lines) -> 17 category modules
  - `llm.rs` (2634 lines) -> 10 sub-modules
  - `harn-lsp/main.rs` (3150 lines) -> 7 modules
  - `harn-cli/main.rs` (1684 lines) -> 5 command modules
- Updated documentation for HTTP builtins (added PUT, PATCH, DELETE),
  provider configuration, cost tracking, and mock HTTP
- Added contributor setup guide and provider config to README

## v0.4.12

### Added

- Data-driven provider configuration via `providers.toml`
- 6 new LLM introspection builtins: `llm_resolve_model`,
  `llm_infer_provider`, `llm_model_tier`, `llm_healthcheck`,
  `llm_providers`, `llm_config`
- Debug logging for provider config loading

## v0.4.11

### Added

- Interfaces with implicit satisfaction (Go-style)
- Generic constraints: `where T: Interface`
- Runtime shape validation for function parameters
- Try-expression: `try { expr }` returns `Result`
- Regex capture groups with named group support
- Spread operator in function calls: `f(...args)`

## v0.4.10

### Added

- `Result` type with `Ok`, `Err`, `unwrap`, `unwrap_or`, `unwrap_err`
- `impl` blocks for attaching methods to structs
- Type narrowing: nil removed from union types after `!= nil` checks
- Stack traces in error messages
- Postfix `?` operator for Result unwrapping

## v0.4.9

### Changed

- Removed bridge `llm_call`/`llm_stream` — native VM handles all LLM calls

## v0.4.8 - v0.4.5

### Added

- Native metadata builtins
- Bridge fixes and conformance tests
- Default function arguments
- `finally` blocks in try/catch
- `select` statement for channel multiplexing
