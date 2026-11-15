# Migrate typed tool clients

Use this guide when a client relied on the earlier schema-only projection of a
typed Harn tool. Prepared wire contracts now validate and construct the declared
runtime value before calling its handler. Invalid return values are withheld.

## Regenerate the contract

Compile the contract from the same source and selection used by the server:

```sh
harn codegen ir --file server.harn --surface exports > contract.json
harn codegen generate --file server.harn --plugin harn-codegen-backend --out generated
```

Replace `harn-codegen-backend` with an installed backend that supports
`harn-codegen/1.0`. Replace generated clients and native handlers together.
A native binding refuses mismatched catalog, binding, or IR digests before
invoking a handler.

## Update hand-written values

Preserve these wire forms when constructing requests or reading responses.
The published schema is authoritative for each selected tool.

| Declared Harn type | Client migration |
| --- | --- |
| `bytes` | Send and accept the closed object `{"$bytes_b64":"AAE="}`. Its value is canonical base64. The old nonstandard JSON Schema type `bytes` is replaced by the envelope's valid object schema. |
| `decimal` | Send and accept an exact canonical decimal string, such as `"1.2300"`. Do not convert it through a JSON number; the string retains precision and scale. |
| `set<T>` | Keep the JSON array representation, but remove duplicate Harn values. The handler now receives a set rather than a list. Decimal scale differences do not make distinct set members. |
| Structs | Keep the closed object of declared fields. Harn now constructs the nominal struct before handler entry; extra fields are rejected. |
| Enums | Use the declared `enum`, `variant`, and positional `fields` envelope. A nullary variant has `fields: []`. Ordinary objects do not substitute for a nominal enum. |
| Tuples | Use an array with the exact declared length and element positions. An empty tuple is `[]`. |
| Optional fields | Omit an absent field. Send `null` only when its declared type accepts null; an explicit null no longer selects a parameter default. |
| `int` | Preserve a signed 64-bit integer token. Do not round it through a floating-point client value. Tokens `1.0`, `1e0`, and `-0` are not integer tokens. |
| Unions and intersections | Follow the prepared schema. An ambiguous union is refused during preparation rather than published as an unconstrained value. |

A field declared as an ordinary dictionary remains dictionary data even when
it contains a reserved-looking key. Apply a byte or enum envelope only where
the declared wire type requires one.

## Verify the boundary

Exercise one valid request through the deployed adapter and check the exact
response, including byte content and decimal strings. Then send a malformed
value and verify that the handler does not run. A response that violates the
declared return type must produce a contract failure without publishing that
value as success.

Unsupported runtime-only types now fail with a source-located diagnostic.
Change the public signature to a supported wire type rather than relying on
an empty schema or a generic JSON conversion. See the
[code generation reference](./code-generation.md) for the portable contract and
[native tool bindings reference](./native-tool-bindings.md) for binding checks.
