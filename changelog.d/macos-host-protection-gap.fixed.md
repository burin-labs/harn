- A macOS spawn refused because the host fails the environment-protection prerequisite now reports
  `availability: host_protection_unavailable` with a `host_protection_gap` (`unrestricted_dtrace`,
  `development_kernel`, or `kernel_configuration_unreadable`). A Mac with System Integrity Protection
  disabled used to read as having no `sandbox-exec`.
