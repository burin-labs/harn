ARM64 Linux, macOS and Windows release builds use thin LTO with 16 codegen units again, because their fat-LTO
builds ran out of memory or past 100 minutes. x86_64 Linux keeps fat LTO.
