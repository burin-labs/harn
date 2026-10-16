require("../../../scripts/ci/rust_cache_generation.cjs")
  .start()
  .catch((error) => {
    console.error(error.message);
    process.exitCode = 1;
  });
