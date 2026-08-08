# HARN-LNT-068 — unknown prompt-template filter

A `.harn.prompt` or `.prompt` expression names a filter the template engine
does not implement. Filter names are valid identifiers, so this is a lint error
rather than a template parse error.

```harn-prompt,ignore
Hello {{ name | uppr }}
```

## How to fix

Replace the name with one of the built-in filters. Near misses include a
machine-applicable suggestion, so `harn lint --fix` can repair `uppr` to
`upper`.

```harn-prompt
Hello {{ name | upper }}
```
