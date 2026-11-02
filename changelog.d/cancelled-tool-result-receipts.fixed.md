Cancelled native tool calls now receive a repair message before the terminal record and a matching raw tool-result
receipt, including when cleanup outlives the original transcript scope. The receipt identifies the harness as its author,
so transcript readers can distinguish cancellation repair from a tool's own failure.
