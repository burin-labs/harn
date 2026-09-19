# HARN-TYP-030: Predicate input must be closed serializable data

Predicate evaluation sends only the declared input to a model and records its
type in the site manifest. The input must be a typed record, tuple, list, string
map, primitive, or union of those types. Functions, capability handles, open
records, recursive types, and gradual `any`, `unknown`, `dict`, or `list` values
do not define that boundary.

Validate external data against a closed schema first. Pass the resulting value,
not a callback, capability, or the surrounding conversation. The policy must
also have a closed record type. Runtime admission separately validates finite
numbers, size, model options, and resource limits.
