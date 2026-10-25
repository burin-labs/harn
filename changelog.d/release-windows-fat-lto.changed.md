The Windows release binary is built with fat LTO and one codegen unit, like x86_64 Linux. ARM64 Linux and macOS
stay on thin LTO, because their fat-LTO builds ran out of memory or past the 135-minute build timeout.
