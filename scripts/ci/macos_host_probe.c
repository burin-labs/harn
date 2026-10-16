/* Temporary #10045 instrumentation. Raw host prerequisites, no fallback policy. */
#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/sysctl.h>
#include <unistd.h>

extern int csr_get_active_config(uint32_t *config);

int main(void) {
  int development = -1;
  size_t size = sizeof(development);
  errno = 0;
  int sysctl_result = sysctlbyname("kern.development", &development, &size, NULL, 0);
  int sysctl_errno = errno;
  uint32_t config = 0;
  errno = 0;
  int csr_result = csr_get_active_config(&config);
  printf("sandbox_exec_access=%d\n", access("/usr/bin/sandbox-exec", X_OK));
  printf("kern_development_result=%d errno=%d bytes=%zu value=%d\n",
         sysctl_result, sysctl_errno, size, development);
  printf("csr_result=%d errno=%d active_config=0x%x unrestricted_dtrace=%d\n",
         csr_result, errno, config, (config & 0x20) != 0);
  return 0;
}
