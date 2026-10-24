- **Test stability.** The remaining lease-release assertions in the language server, the serve
  prompt catalog and the hostlib artifact guard wait out descriptors a concurrent child spawn still
  holds, instead of failing the test on the first try.
