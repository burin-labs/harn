- Release binaries now build with fat LTO and one codegen unit on every target.
  The x86_64 Linux `harn` binary is about 15% smaller (277.8 MB to 234.7 MB in
  a paired local build), with no change to commands or startup latency.
