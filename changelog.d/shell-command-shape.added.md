`std/command` adds `shell_command_shape(source, shell)`, which reads a shell
line's operator structure with the grammars the command policy already uses:
each chain member, the operator joining it (`&&`, `||`, `;`, `&`, newline), and
its pipeline stages with unquoted words. Quoted operators stay argument text.
POSIX shells and PowerShell are measured; unparseable lines, unknown shells and
`cmd` are reported as unmeasured with no segments.
