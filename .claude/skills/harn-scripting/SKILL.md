---
name: harn-scripting
description: Pointer skill - fetch the harn-* inner skill matching your task via the local harn binary
---

Before writing or editing `.harn` code, list the available inner skills with
`harn skills list --json` and fetch the narrowest match with
`harn skills get <name> --full`. The canonical content ships in the binary
itself, so the docs always match the binary version in use.
