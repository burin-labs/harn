New lint `capability-parameter-name` (HARN-LNT-073) reports a parameter typed as
a narrow capability handle that is not named for the capability it carries —
most often `harness: HarnessNet`, which reads as the root handle at every call
site and hides the attenuation the narrow type performs.

It carries a machine-applicable rename, so
`harn fix --apply --safety surface-changing` rewrites the parameter and every
reference to it inside the function. Harn arguments are positional, so no call
site moves. The lint stays quiet whenever the rename is not provably safe from
the function alone: when the capability's name is already bound there, when a
nested callable rebinds either name, or on a rest parameter. A dict key that
shares the parameter's name is a record field, not a reference, and is left
alone.

This also completes the HARN-LNT-069 attenuation repair, which narrows a
parameter's type but reuses its existing name.
