# Source file-length baseline

Each `.lines` shard mirrors one Rust or stdlib Harn source path and contains
its exact logical-line count. Shards exist only for files already above the
1,500-line ceiling when the ratchet was introduced.

After splitting or shrinking a grandfathered file, run:

```sh
make update-source-file-length-baseline
```

The updater tightens or removes existing shards. It deliberately refuses to
grandfather newly oversized files. If exceptional growth is unavoidable,
edit that source's shard explicitly so the increase is visible in review.
