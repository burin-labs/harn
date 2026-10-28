# Recover a blocked spend scope

Use this procedure when a durable provider allowance with `stop_on_unknown_usage = true`
refuses work after a process crash. You need authority over the host's spend policy,
all processes using the scope, and its retained ledger. Keep the original scope blocked
until you can account for its outstanding attempts.

## Stop dispatch and preserve the evidence

1. Stop every process that can dispatch against the affected scope, including ACP
   servers and restarted workers. Disable their automatic restart. An expired client
   or disconnected stream does not prove the provider stopped processing the request.
2. Preserve a consistent SQLite backup, the policy file, provider request identifiers,
   request limits, pricing evidence, and available usage or billing receipts. Include
   every physical dispatch and retry. Keep the originals outside agent-writable roots.
3. Have an authorized host read `MachineSpendQuota::receipt()` from the retained ledger.
   Record the scope, policy limits, snapshot time and reset times alongside the receipt.
   `lifetime_unsettled_attempts` counts attempts without settlement;
   `lifetime_unpriced_attempts` counts settled attempts without measured usage.
   Neither means a measured zero. The receipt's remaining amounts can be positive
   while strict admission still refuses every new reservation.

## Establish the maximum prior charge

Reconcile provider receipts with each physical dispatch. For an attempt without
complete measured usage, retain its full reservation as possible spend. Confirm that
the reservation bounds that request's maximum charge using its prices, input and
output limits, and any other billable work. A timeout, cancellation, missing response,
or elapsed day is insufficient evidence to release a hold.

`lifetime_reserved_microusd` includes holds and conservative amounts retained after
settlement. Do not add `lifetime_actual_known_microusd` to it: measured charges are
already covered by that total.
Do not substitute the known total for the reserved total, which would discard the
unknown attempts. Check daily and monthly limits separately from the lifetime limit.

If a bound cannot be established, or `contract_broken` is true, keep dispatch stopped
until the provider evidence resolves the uncertainty. Increasing limits through
`MachineSpendQuota::update_policy` changes a ceiling and records approval; it does
not settle an attempt or remove the strict refusal.

## Resume with an explicitly bounded replacement scope

Harn currently exposes no operator CLI command or public host API to reconcile an
orphaned reservation in place. Do not delete attempts, edit settlement flags, disable
the stored strict policy, or recreate the ledger to make the refusal disappear.

When the prior-charge bound is defensible and more work is authorized:

1. Retire the original scope from every dispatcher. Preserve its ledger and holds;
   prevent old workers from using that authority again.
2. Record the maximum prior charge and the remaining authorization. With valid
   reservation bounds and no contract violation, use the smallest non-null value
   among the original receipt's daily, monthly, and lifetime remaining amounts as
   a conservative cap for this recovery. Take the snapshot after dispatch has stopped.
   A missing receipt or an unproven bound cannot establish this cap.
3. Create a new, distinct scope with `stop_on_unknown_usage = true` and a lifetime
   ceiling no greater than that cap. Retain any tighter destination or host limits.
   Record the old and new scope identifiers, evidence, cap, and authorizing operator
   together. Renaming a scope alone transfers none of its prior charges.
4. Install the replacement policy in every authorized dispatcher before restarting
   them. Observe one completed request with measured usage and its durable receipt
   before resuming additional work. A competing reservation is still refused;
   callers must handle that refusal explicitly.

For example, an original lifetime ceiling of 2,000,000 micro-USD with 700,000 in
retained settled amounts and 300,000 in bounded unknown holds has 1,000,000 remaining. If
the daily and monthly remaining amounts are 400,000 and 800,000, cap the replacement
scope's lifetime allowance at 400,000. Keep the old 300,000 hold and its unknown
count intact. This conservatively preserves all three limits without claiming the
interrupted request cost zero or that the original scope was reconciled.
