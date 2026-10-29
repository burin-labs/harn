- Read provider and MCP Server-Sent Events through one shared framer built on
  `eventsource-stream`. The OpenAI-compatible stream reader no longer drops
  `data:{...}` frames written without a space, and every reader now joins a
  payload split across several `data:` lines and accepts CRLF line ends.
