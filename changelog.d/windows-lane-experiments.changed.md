- CI: the Windows lane gains two independently-toggleable, default-off
  experiments — `HARN_WINDOWS_LLD=on` links with the toolchain-bundled
  rust-lld instead of MSVC link.exe, and `HARN_WINDOWS_DEVDRIVE=on` places
  Cargo state on a ReFS Dev Drive. Both mirror byte-identically across the
  nightly cache writer and the CI consumer; `MEASUREMENT.md` documents the
  A/B protocol, predictions, and refuted alternatives.
