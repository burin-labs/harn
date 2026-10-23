- `harn serve site --ready-file PATH` publishes the actual bound URL and process ID as JSON,
  including when the operating system chooses the port. Existing files and failed startup
  never publish a new ready receipt.
