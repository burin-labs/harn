Interpreter execution services now use one required runtime owner across VM
construction, child execution, and baseline instantiation. Harness dispatch no
longer depends on a process-global installation slot; policy checks remain
mandatory on each builtin dispatch path.

Child interpreters retain pool, credential-provider, and tool-preparation
context, while independent sessions inherit restrictions without reusing
another invocation's authority. Native host access, evidence assembly, step
budget decisions, and timed-entry rollback live above interpreter mechanics.
