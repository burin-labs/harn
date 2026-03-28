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
| `json_validate(data, schema)` | data: any, schema: dict | bool | Validate data against a schema. Returns `true` if valid, throws with details if not |
| `json_extract(text, key?)` | text: string, key: string (optional) | value | Extract JSON from text (strips markdown code fences). If key given, returns that key's value |

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

### json_validate schema format

The schema is a plain Harn dict (not JSON Schema). Supported keys:

| Key | Type | Description |
|---|---|---|
| `type` | string | Expected type: `"string"`, `"int"`, `"float"`, `"bool"`, `"list"`, `"dict"`, `"any"` |
| `required` | list | List of required key names (for dicts) |
| `properties` | dict | Dict mapping property names to sub-schemas (for dicts) |
| `items` | dict | Schema to validate each item against (for lists) |

Example:

```harn
let schema = {
  type: "dict",
  required: ["name", "age"],
  properties: {
    name: {type: "string"},
    age: {type: "int"},
    tags: {type: "list", items: {type: "string"}}
  }
}
json_validate(data, schema)  // throws if invalid
```

### json_extract

Extracts JSON from LLM responses that may contain markdown code fences
or surrounding prose. Handles `` ```json ... ``` ``, `` ``` ... ``` ``,
and bare JSON with surrounding text.

```harn
let response = llm_call("Return JSON with name and age")
let data = json_extract(response)         // parse, stripping fences
let name = json_extract(response, "name") // extract just one key
```

## String functions

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `len(value)` | value: string, list, or dict | int | Length of string (chars), list (items), or dict (keys) |
| `trim(str)` | str: string | string | Remove leading and trailing whitespace |
| `lowercase(str)` | str: string | string | Convert to lowercase |
| `uppercase(str)` | str: string | string | Convert to uppercase |
| `split(str, sep)` | str: string, sep: string | list | Split string by separator |
| `starts_with(str, prefix)` | str: string, prefix: string | bool | Check if string starts with prefix |
| `ends_with(str, suffix)` | str: string, suffix: string | bool | Check if string ends with suffix |
| `contains(str, substr)` | str: string, substr: string | bool | Check if string contains substring. Also works on lists |
| `replace(str, old, new)` | str: string, old: string, new: string | string | Replace all occurrences |
| `join(list, sep)` | list: list, sep: string | string | Join list elements with separator |
| `substring(str, start, len?)` | str: string, start: int, len: int | string | Extract substring from start position |
| `format(template, ...)` | template: string, args: any | string | Format string with `{}` placeholders |

## Path functions

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `dirname(path)` | path: string | string | Directory component of path |
| `basename(path)` | path: string | string | File name component of path |
| `extname(path)` | path: string | string | File extension including dot (e.g., `.harn`) |
| `path_join(parts...)` | parts: strings | string | Join path components |

## File I/O

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `read_file(path)` | path: string | string | Read entire file as UTF-8 string. Throws on failure |
| `write_file(path, content)` | path: string, content: string | nil | Write string to file. Throws on failure |
| `render(path, bindings?)` | path: string, bindings: dict | string | Read a template file and replace `{{key}}` placeholders with values from bindings dict. Without bindings, just reads the file |

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

## Persistent store

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `store_get(key)` | key: string | any | Retrieve value from store, nil if missing |
| `store_set(key, value)` | key: string, value: any | nil | Store value, auto-saves to `.harn/store.json` |
| `store_delete(key)` | key: string | nil | Remove key from store |
| `store_list()` | none | list | List all keys (sorted) |
| `store_save()` | none | nil | Explicitly flush store to disk |
| `store_clear()` | none | nil | Remove all keys from store |

The store is backed by `.harn/store.json` relative to the script's
directory. The file is created lazily on first `store_set`. In bridge mode,
the host can override these builtins.

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

### Auto-connecting MCP servers via harn.toml

Instead of calling `mcp_connect` manually, you can declare MCP servers in
`harn.toml`. They will be connected automatically before the pipeline executes
and made available through the global `mcp` dict.

Add a `[[mcp]]` entry for each server:

```toml
[[mcp]]
name = "filesystem"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]

[[mcp]]
name = "github"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]
```

Each entry requires:

| Field | Type | Description |
|---|---|---|
| `name` | string | Identifier used to access the client (e.g., `mcp.filesystem`) |
| `command` | string | Executable to spawn |
| `args` | list of strings | Command-line arguments (default: empty) |

The connected clients are available as properties on the `mcp` global dict:

```harn
pipeline default() {
  let tools = mcp_list_tools(mcp.filesystem)
  println(tools)

  let result = mcp_call(mcp.github, "list_issues", {repo: "harn"})
  println(result)
}
```

If a server fails to connect, a warning is printed to stderr and that
server is omitted from the `mcp` dict. Other servers still connect
normally. The `mcp` global is only defined when at least one server
connects successfully.
