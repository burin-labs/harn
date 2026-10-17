# Run an experiment inside one allowance

Use this example to allocate one allowance across actor and grader calls in
two experiment arms. Run it from a Harn checkout with `OPENAI_API_KEY` exported
and `harn` on `PATH`:

```bash
harn run --standalone --no-sandbox examples/conservative-experiment.harn -- --live
```

The command makes four direct OpenAI text requests. It disables the process
sandbox so the child Harn processes can reach the provider. Use it only with
the checked-in example. Set `HARN_BIN` to an absolute executable path when
testing a particular build; the parent uses that executable for every child.

The initial allowance is $2.40. Each arm receives $0.60 for its actor and $0.60
for its grader. The actor repairs a Python addition function; the grader reads
the actor's answer. Each allocation starts one independent process with
`budget.admission: "conservative"` before its first provider call. The parent
checks the allocation sum in integer millionths of a dollar before launching
any child. Unused allowance stays with that allocation and isn't reassigned.

Read the terminal JSON report. A successful run contains four `runs`, each
with a distinct arm/role pair, one measured provider call, complete actual
usage, and a conservative ceiling of `"0.6"`. Every child must have zero
in-flight and uncertain reservation, no denied attempts, and settlement at
or below its allocation. `cost_usd` reports actual priced usage;
`settled_upper_usd` reports the conservative charge. Keep them separate.

If a child fails or returns incomplete usage, the parent stops without
launching the remaining allocations. Don't restart the whole experiment
against the same $2.40 allowance: each new process starts a fresh ledger.
Account for the earlier run before allocating a new attempt. Use a
[machine spend quota](../llm/llm_call.md#machine-spend-quota-for-native-hosts)
when the ceiling must survive process restarts.

To check the refusal path without a provider request, give a single actor
only $0.01:

```bash
harn run --standalone --no-sandbox examples/conservative-experiment.harn -- --call actor 10000 'Return hello'
```

The report must contain `ok: false`, `insufficient_allowance`, one denied
attempt, zero provider calls, and zero settled or uncertain allowance. The
supported request's full-context bound exceeds $0.01 even though its prompt
is short. A successful four-call run is the positive control for that refusal.

See [conservative admission](../llm/llm_call.md#conservative-admission) for
supported billing shapes and settlement rules. These calls demonstrate
allocation and accounting, without establishing actor or grader accuracy.
