- **The default `worktree` sandbox now confines and runs subprocesses on stock
  Ubuntu 22.04 and 24.04, Debian 12, and RHEL 9 kernels.** Linux selected
  Landlock only at ABI 6 and otherwise required bubblewrap, so on these
  systems every confined spawn was refused unless bubblewrap was installed and
  user namespaces were allowed. Harn now negotiates the kernel's Landlock ABI
  through the `landlock` crate and uses any ABI: the seccomp filter refuses
  truncation by path below ABI 3, and only the serve-only Unix socket grant
  still needs ABI 6. `harn doctor` and `harn doctor sandbox` list each
  remaining difference as a mechanism limit. Bubblewrap now renders the
  default files-only `/proc` grant as the sandbox's private procfs instead of
  refusing every launch.
