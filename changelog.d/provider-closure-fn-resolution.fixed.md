Registered reminder providers and session/lifecycle hooks now resolve sibling module `pub fn`s from inside
their closures even after the VM that registered them is torn down. Previously the closure's module function
table was held only via a `Weak` owned by that VM's module cache, so a provider/hook fired from a later VM
misdispatched the sibling call to the host bridge (`host bridge tool '<fn>' is not implemented`), killing the
agent turn. The registered closure now pins its module scope for its retained lifetime.
