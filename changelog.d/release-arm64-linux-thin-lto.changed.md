ARM64 Linux release builds use thin LTO with 16 codegen units again, because its fat-LTO link needs about 30 GB
and only finishes by swapping. Other targets keep fat LTO.
