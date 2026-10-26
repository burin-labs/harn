- **CI recovery tolerates transient metadata failures.** Explicit GitHub server errors retry within a bounded read budget,
  and incomplete or malformed issue listings stop recovery before any issue changes.
