# Harn fixture

A tiny Markdown document for the AST edit fixtures.

## Usage

Run the edit primitives against this file:

- `apply_node` replaces a node span.
- `insert_at_anchor` adds a sibling or child.

```rust
fn main() {
    println!("hello");
}
```

## See also

The [language coverage](../README.md) contract.
