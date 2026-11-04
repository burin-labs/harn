# Bounded scope attenuation verification

The production regression is
`crates/harn-vm/tests/scope_attenuation_bounded.rs`. It invokes
`ActorChain::validate_scope_attenuation` for every assignment of four scope
symbols to an origin and zero through three delegated actors, in all three
policy modes and with both alert settings: **419,424 calls**.

Its independent bit-membership oracle checks acceptance, the first invalid
root-to-current edge, error subjects, mode, full scope sets, sorted extra
scopes, and that validation leaves the chain unchanged. Coverage counts make
missing depths or over-restrictive assumptions fail. Empty, equal, narrower,
wider and incomparable sets are included; Off deliberately allows widening.

Run through the repository's normal test interface:

```sh
HARN_TEST_ONE_PACKAGE=harn-vm HARN_TEST_ONE_BINARY=scope_attenuation_bounded HARN_TEST_ONE_NAME=bounded_scope_attenuation_matches_membership_oracle make test-one
```

## Optional algebra proof

With an already installed Z3, inspect the mathematical model from the repo root:

```sh
z3 -smt2 experiments/formal-scope-attenuation/scope_algebra.smt2
```

The expected transcript has eight UNSAT counterexample searches, an Off-mode
widening witness, and three SAT mutation witnesses: removing inclusion, removing
strictness, and skipping the origin edge. Unknown, missing or errored results
are not evidence of success. This optional command is not a CI gate; the Rust
test is the maintained connection to production code. No solver installation
or new CI dependency is added.

This is a finite mathematical abstraction, **not Rust MIR verification**.
It trusts Z3 and the encoding, plus the refinement from normalized
`BTreeSet<String>` membership and the Rust loop to the bitset chain. The
Rust test exercises that connection over the stated domain; a source hash
alone does not prove it. Four-bit cardinalities and rank sums fit without
overflow in the encoded bounds.

When changing the oracle, check its sensitivity by separately removing the
production subset condition, removing strictness, and skipping the origin
edge in an owned worktree. The exact Rust test must fail at its intended
assertion for each mutant, then pass with the source restored. A compilation
failure is not a successful negative control.

## Limits and next step

Neither check proves arbitrary strings, Unicode/whitespace normalization,
malformed input, longer chains, token signatures, identity/issuer authenticity,
grant authenticity, alert delivery, or enforcement at every ingress. Policy
alert flags are covered only for this pure validator's return values.

[Kani](https://github.com/model-checking/kani/blob/main/docs/src/getting-started.md)
is a possible next step for proof over actual Rust, subject to a measured
compatibility and resource check of the VM dependency graph and collections.
Its [unwinding assertions](https://model-checking.github.io/kani/tutorial-loop-unwinding.html)
must pass before reporting any bounded result. No Kani result is claimed here.
The payoff of this first step is a cheap exhaustive regression tied to the
production function, with explicit assumptions and falsifiers.
