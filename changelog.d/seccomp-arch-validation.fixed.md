- **Linux sandbox: validate the syscall ABI before the syscall number.** The
  seccomp-bpf allowlist matched `seccomp_data.nr` without first checking
  `seccomp_data.arch`, so a confined x86-64 process could re-enter the kernel
  through the i386 compat gate (`int $0x80`) — where the same numbers name
  different syscalls — and reach calls the policy withholds. Number 26 is
  `msync`, which every profile permits; i386 number 26 is `ptrace`, which the
  allowlist deliberately excludes. `CONFIG_IA32_EMULATION` is enabled by
  default across mainstream distro kernels and needs no 32-bit binary or
  libraries to reach, so the confinement was bypassable as shipped. Filter
  construction now goes through `seccompiler`, which prefixes every program
  with an architecture check that kills the process on mismatch. The program is
  also compiled ahead of `fork` rather than inside `pre_exec`, where its
  allocation was not async-signal-safe.
