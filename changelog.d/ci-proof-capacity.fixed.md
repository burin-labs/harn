- **CI: stop the behavior-payload jobs from running out of disk.** The
  `Rust test` and `Rust security proof` jobs kept the ~8.4 GiB compressed
  behavior bundle on disk after extracting an equally large copy, and nextest
  then unpacked a third, uncompressed copy — a total that hosted runners do
  not reliably fit, failing runs with `No space left on device`. Both jobs now
  drop the compressed bundle as soon as the checksummed restore has consumed
  it. The behavior-artifact scripts also compare archive listings under
  `LC_ALL=C`, so their gate tests no longer fail on machines whose locale
  sorts uppercase names differently.
