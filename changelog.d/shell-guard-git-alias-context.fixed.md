The shell guard resolves builtin commands from the selected Git executable and
refuses aliases whose configuration can change within the guarded payload.
Builtin and alias queries share the executable context, and preflight does not
execute payload paths outside the hook's trusted PATH.
