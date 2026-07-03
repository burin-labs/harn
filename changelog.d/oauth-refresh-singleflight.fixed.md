Added storage-scoped OAuth refresh locking so std/oauth clients re-read tokens inside
a single-flight transaction before spending refresh grants.
