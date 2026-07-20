- Dropped the unmaintained `fs2` dependency in favour of the standard library's
  file-locking API, stable since Rust 1.89. Contention is now detected through
  the typed `std::fs::TryLockError::WouldBlock` variant instead of comparing raw
  OS error codes, and available disk space is reported through `sysinfo`.
