# Builtin functions

Complete reference for all built-in functions available in Harn.

## Output

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `log(msg)` | msg: any | nil | Print with `[harn]` prefix and newline |
| `print(msg)` | msg: any | nil | Print without prefix or newline |
| `println(msg)` | msg: any | nil | Print with newline, no prefix |

## Type conversion

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `type_of(value)` | value: any | string | Returns type name: `"int"`, `"float"`, `"string"`, `"bool"`, `"nil"`, `"list"`, `"dict"`, `"closure"`, `"taskHandle"`, `"duration"`, `"enum"`, `"struct"` |
| `to_string(value)` | value: any | string | Convert to string representation |
| `to_int(value)` | value: any | int or nil | Parse/convert to integer. Floats truncate, bools become 0/1 |
| `to_float(value)` | value: any | float or nil | Parse/convert to float |

## JSON

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `json_parse(str)` | str: string | value | Parse JSON string into Harn values. Throws on invalid JSON |
| `json_stringify(value)` | value: any | string | Serialize Harn value to JSON. Closures and handles become `null` |

Type mapping:

| JSON | Harn |
|---|---|
| string | string |
| integer | int |
| decimal/exponent | float |
| true/false | bool |
| null | nil |
| array | list |
| object | dict |

## File I/O

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `read_file(path)` | path: string | string | Read entire file as UTF-8 string. Throws on failure |
| `write_file(path, content)` | path: string, content: string | nil | Write string to file. Throws on failure |

## Environment and system

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `env(name)` | name: string | string or nil | Read environment variable |
| `timestamp()` | none | float | Unix timestamp in seconds with sub-second precision |
| `exit(code)` | code: int (default 0) | never | Terminate the process |

## Regular expressions

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `regex_match(pattern, text)` | pattern: string, text: string | list or nil | Find all non-overlapping matches. Returns nil if no matches |
| `regex_replace(pattern, replacement, text)` | pattern: string, replacement: string, text: string | string | Replace all matches. Throws on invalid regex |

## HTTP

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `http_get(url)` | url: string | string | GET request, returns response body |
| `http_post(url, body, headers?)` | url: string, body: string, headers: dict (optional) | string | POST request with optional headers dict |

Both throw on network errors.

## Interactive input

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `prompt_user(msg)` | msg: string (optional) | string | Display message, read line from stdin |

## Async and timing

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `sleep(duration)` | duration: int (ms) or duration literal | nil | Pause execution |

## Concurrency primitives

### Channels

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `channel(name?)` | name: string (default `"default"`) | dict | Create a channel with `name`, `type`, and `messages` fields |
| `send(ch, value)` | ch: dict, value: any | nil | Send a value to a channel |
| `receive(ch)` | ch: dict | any | Receive a value from a channel |

### Atomics

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `atomic(initial?)` | initial: any (default 0) | dict | Create an atomic value |
| `atomic_get(a)` | a: dict | any | Read the current value |
| `atomic_set(a, value)` | a: dict, value: any | dict | Returns new atomic with updated value |
| `atomic_add(a, delta)` | a: dict, delta: int | dict | Returns new atomic with incremented value |

## LLM

See [LLM calls and agent loops](llm-and-agents.md) for full documentation.

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `llm_call(prompt, system?, options?)` | prompt: string, system: string, options: dict | string | Single LLM request |
| `agent_loop(prompt, system?, options?)` | prompt: string, system: string, options: dict | string | Multi-turn agent loop with `##DONE##` sentinel |

## MCP (Model Context Protocol)

Connect to external tool servers using the
[Model Context Protocol](https://modelcontextprotocol.io). Supports stdio
transport (spawns a child process).

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `mcp_connect(command, args?)` | command: string, args: list | mcp\_client | Spawn an MCP server and perform the initialize handshake |
| `mcp_list_tools(client)` | client: mcp\_client | list | List available tools from the server |
| `mcp_call(client, name, arguments?)` | client: mcp\_client, name: string, arguments: dict | string or list | Call a tool and return the result |
| `mcp_server_info(client)` | client: mcp\_client | dict | Get connection info (`name`, `connected`) |
| `mcp_disconnect(client)` | client: mcp\_client | nil | Kill the server process and release resources |

Example:

```harn
let client = mcp_connect("npx", ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"])
let tools = mcp_list_tools(client)
println(tools)

let result = mcp_call(client, "read_file", {"path": "/tmp/hello.txt"})
println(result)

mcp_disconnect(client)
```

Notes:

- `mcp_call` returns a string when the tool produces a single text block,
  a list of content dicts for multi-block results, or nil when empty.
- If the tool reports `isError: true`, `mcp_call` throws the error text.
- `mcp_connect` throws if the command cannot be spawned or the initialize
  handshake fails.
