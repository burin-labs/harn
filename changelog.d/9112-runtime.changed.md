Interpreter execution services now use one required runtime owner across VM
construction, child execution, and baseline instantiation. Harness dispatch no
longer depends on a process-global installation slot; policy checks remain
mandatory on each builtin dispatch path.
