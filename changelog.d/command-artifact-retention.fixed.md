- **Command artifact retention.** Harn now sweeps old `harn-command-*`
  temp directories by count as well as age, preventing long agent/eval runs
  from exhausting temp-dir quotas with thousands of small stdout artifacts.
