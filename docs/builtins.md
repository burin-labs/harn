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

## Runtime shape validation

Function parameters with structural type annotations (shapes) are validated
at runtime. If a dict or struct argument is missing a required field or has
the wrong field type, a descriptive error is thrown before the function
body executes.

```harn
fn greet(u: {name: string, age: int}) {
  println("${u.name} is ${u.age}")
}

greet({name: "Alice", age: 30})   // OK
greet({name: "Alice"})            // Error: parameter 'u': missing field 'age' (int)
```

See [Error handling -- Runtime shape validation errors](error-handling.md#runtime-shape-validation-errors)
for more details.

## Result

Harn has a built-in `Result` type for representing success/failure values
without exceptions. `Ok` and `Err` create `Result.Ok` and `Result.Err`
enum variants respectively. When called on a non-Result value, `unwrap`
and `unwrap_or` pass the value through unchanged.

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `Ok(value)` | value: any | Result.Ok | Create a Result.Ok value |
| `Err(value)` | value: any | Result.Err | Create a Result.Err value |
| `is_ok(result)` | result: any | bool | Returns true if value is Result.Ok |
| `is_err(result)` | result: any | bool | Returns true if value is Result.Err |
| `unwrap(result)` | result: any | any | Extract Ok value. Throws on Err. Non-Result values pass through |
| `unwrap_or(result, default)` | result: any, default: any | any | Extract Ok value. Returns default on Err. Non-Result values pass through |
| `unwrap_err(result)` | result: any | any | Extract Err value. Throws on non-Err |

Example:

```harn
let good = Ok(42)
let bad = Err("something went wrong")

println(is_ok(good))             // true
println(is_err(bad))             // true

println(unwrap(good))            // 42
println(unwrap_or(bad, 0))       // 0
println(unwrap_err(bad))         // something went wrong
```

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

## Math

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `abs(n)` | n: int or float | int or float | Absolute value |
| `ceil(n)` | n: float | int | Ceiling (rounds up). Ints pass through unchanged |
| `floor(n)` | n: float | int | Floor (rounds down). Ints pass through unchanged |
| `round(n)` | n: float | int | Round to nearest integer. Ints pass through unchanged |
| `sqrt(n)` | n: int or float | float | Square root |
| `pow(base, exp)` | base: number, exp: number | int or float | Exponentiation. Returns int when both args are int and exp is non-negative |
| `min(a, b)` | a: number, b: number | int or float | Minimum of two values. Returns float if either argument is float |
| `max(a, b)` | a: number, b: number | int or float | Maximum of two values. Returns float if either argument is float |
| `random()` | none | float | Random float in [0, 1) |
| `random_int(min, max)` | min: int, max: int | int | Random integer in [min, max] inclusive |

### Trigonometry

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `sin(n)` | n: float | float | Sine (radians) |
| `cos(n)` | n: float | float | Cosine (radians) |
| `tan(n)` | n: float | float | Tangent (radians) |
| `asin(n)` | n: float | float | Inverse sine |
| `acos(n)` | n: float | float | Inverse cosine |
| `atan(n)` | n: float | float | Inverse tangent |
| `atan2(y, x)` | y: float, x: float | float | Two-argument inverse tangent |

### Logarithms and exponentials

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `log2(n)` | n: float | float | Base-2 logarithm |
| `log10(n)` | n: float | float | Base-10 logarithm |
| `ln(n)` | n: float | float | Natural logarithm |
| `exp(n)` | n: float | float | Euler's number raised to the power n |

### Constants and utilities

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `pi()` | none | float | The constant pi (3.14159...) |
| `e()` | none | float | Euler's number (2.71828...) |
| `sign(n)` | n: int or float | int | Sign of a number: -1, 0, or 1 |
| `is_nan(n)` | n: float | bool | Check if value is NaN |
| `is_infinite(n)` | n: float | bool | Check if value is infinite |

## Sets

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `set(items?)` | items: list (optional) | set | Create a new set, optionally from a list |
| `set_add(s, value)` | s: set, value: any | set | Add a value to a set, returns new set |
| `set_remove(s, value)` | s: set, value: any | set | Remove a value from a set, returns new set |
| `set_contains(s, value)` | s: set, value: any | bool | Check if set contains a value |
| `set_union(a, b)` | a: set, b: set | set | Union of two sets |
| `set_intersect(a, b)` | a: set, b: set | set | Intersection of two sets |
| `set_difference(a, b)` | a: set, b: set | set | Difference of two sets (elements in a but not in b) |
| `to_list(s)` | s: set | list | Convert a set to a sorted list |

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
| `append_file(path, content)` | path: string, content: string | nil | Append string to file, creating it if it doesn't exist. Throws on failure |
| `copy_file(src, dst)` | src: string, dst: string | nil | Copy a file. Throws on failure |
| `delete_file(path)` | path: string | nil | Delete a file or directory (recursive). Throws on failure |
| `file_exists(path)` | path: string | bool | Check if a file or directory exists |
| `list_dir(path?)` | path: string (default `"."`) | list | List directory contents as sorted list of file names. Throws on failure |
| `mkdir(path)` | path: string | nil | Create directory and all parent directories. Throws on failure |
| `stat(path)` | path: string | dict | File metadata: `{size, is_file, is_dir, readonly, modified}`. Throws on failure |
| `temp_dir()` | none | string | System temporary directory path |
| `render(path, bindings?)` | path: string, bindings: dict | string | Read a template file and replace `{{key}}` placeholders with values from bindings dict. Without bindings, just reads the file |

## Environment and system

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `env(name)` | name: string | string or nil | Read environment variable |
| `timestamp()` | none | float | Unix timestamp in seconds with sub-second precision |
| `exec(cmd, args...)` | cmd: string, args: strings | dict | Execute external command. Returns `{stdout, stderr, status, success}`. Throws if command cannot be spawned |
| `exit(code)` | code: int (default 0) | never | Terminate the process |

## Regular expressions

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `regex_match(pattern, text)` | pattern: string, text: string | list or nil | Find all non-overlapping matches. Returns nil if no matches |
| `regex_replace(pattern, replacement, text)` | pattern: string, replacement: string, text: string | string | Replace all matches. Throws on invalid regex |
| `regex_captures(pattern, text)` | pattern: string, text: string | list | Find all matches with capture group details |

### regex_captures

Returns a list of dicts, one per match. Each dict contains:

- `match` -- the full matched string
- `groups` -- a list of positional capture group values (from `(...)`)
- Named capture groups (from `(?P<name>...)`) appear as additional keys

```harn
let results = regex_captures("(\\w+)@(\\w+)", "alice@example bob@test")
// [
//   {match: "alice@example", groups: ["alice", "example"]},
//   {match: "bob@test", groups: ["bob", "test"]}
// ]
```

Named capture groups are added as top-level keys on each result dict:

```harn
let named = regex_captures("(?P<user>\\w+):(?P<role>\\w+)", "alice:admin")
// [{match: "alice:admin", groups: ["alice", "admin"], user: "alice", role: "admin"}]
```

Returns an empty list if there are no matches. Throws on invalid regex.

## Encoding

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `base64_encode(string)` | string: string | string | Base64 encode a string (standard alphabet with padding) |
| `base64_decode(string)` | string: string | string | Base64 decode a string. Throws on invalid input |

Example:

```harn
let encoded = base64_encode("Hello, World!")
println(encoded)                  // SGVsbG8sIFdvcmxkIQ==
println(base64_decode(encoded))   // Hello, World!
```

## Hashing

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `sha256(string)` | string: string | string | SHA-256 hash, returned as a lowercase hex-encoded string |
| `md5(string)` | string: string | string | MD5 hash, returned as a lowercase hex-encoded string |

Example:

```harn
println(sha256("hello"))  // 2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824
println(md5("hello"))     // 5d41402abc4b2a76b9719d911017c592
```

## Date/Time

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `date_now()` | none | dict | Current UTC datetime as dict with `year`, `month`, `day`, `hour`, `minute`, `second`, `weekday`, and `timestamp` fields |
| `date_parse(str)` | str: string | float | Parse a datetime string (e.g., `"2024-01-15 10:30:00"`) into a Unix timestamp. Extracts numeric components from the string. Throws if fewer than 3 parts (year, month, day) |
| `date_format(dt, format?)` | dt: float, int, or dict; format: string (default `"%Y-%m-%d %H:%M:%S"`) | string | Format a timestamp or date dict as a string. Supports `%Y`, `%m`, `%d`, `%H`, `%M`, `%S` placeholders |

## Testing

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `assert(condition, msg?)` | condition: any, msg: string (optional) | nil | Assert value is truthy. Throws with message on failure |
| `assert_eq(a, b, msg?)` | a: any, b: any, msg: string (optional) | nil | Assert two values are equal. Throws with message on failure |
| `assert_ne(a, b, msg?)` | a: any, b: any, msg: string (optional) | nil | Assert two values are not equal. Throws with message on failure |

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
| `receive(ch)` | ch: dict | any | Receive a value from a channel (blocks until data available) |
| `close_channel(ch)` | ch: channel | nil | Close a channel, preventing further sends |
| `try_receive(ch)` | ch: channel | any or nil | Non-blocking receive. Returns nil if no data available |
| `select(ch1, ch2, ...)` | channels: channel | dict or nil | Wait for data on any channel. Returns `{index, value, channel}` for the first ready channel, or nil if all closed |

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
| `agent_loop(prompt, system?, options?)` | prompt: string, system: string, options: dict | dict | Multi-turn agent loop with `##DONE##` sentinel. Returns `{status, text, iterations, duration_ms, tools_used}` |
| `llm_info()` | — | dict | Current LLM config: `{provider, model, api_key_set}` |
| `llm_usage()` | — | dict | Cumulative usage: `{input_tokens, output_tokens, total_duration_ms, call_count}` |

## Timers

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `timer_start(name?)` | name: string | dict | Start a named timer |
| `timer_end(timer)` | timer: dict | int | Stop timer, prints elapsed, returns milliseconds |
| `elapsed()` | — | int | Milliseconds since process start |

## Structured logging

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `log_json(key, value)` | key: string, value: any | nil | Emit a JSON log line with timestamp |

## Metadata

Project metadata store backed by `.burin/metadata/` sharded JSON files.
Supports hierarchical namespace resolution (child directories inherit
from parents).

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `metadata_get(dir, namespace?)` | dir: string, namespace: string | dict \| nil | Read metadata with inheritance |
| `metadata_set(dir, namespace, data)` | dir: string, namespace: string, data: dict | nil | Write metadata for directory/namespace |
| `metadata_save()` | — | nil | Flush metadata to disk |
| `metadata_stale(project)` | project: string | dict | Check staleness: `{any_stale, tier1, tier2}` |
| `metadata_refresh_hashes()` | — | nil | Recompute content hashes |
| `compute_content_hash(dir)` | dir: string | string | Hash of directory contents |
| `invalidate_facts(dir)` | dir: string | nil | Mark cached facts as stale |

## MCP (Model Context Protocol)

Connect to external tool servers using the
[Model Context Protocol](https://modelcontextprotocol.io). Supports stdio
transport (spawns a child process).

| Function | Parameters | Returns | Description |
|---|---|---|---|
| `mcp_connect(command, args?)` | command: string, args: list | mcp\_client | Spawn an MCP server and perform the initialize handshake |
| `mcp_list_tools(client)` | client: mcp\_client | list | List available tools from the server |
| `mcp_call(client, name, arguments?)` | client: mcp\_client, name: string, arguments: dict | string or list | Call a tool and return the result |
| `mcp_list_resources(client)` | client: mcp\_client | list | List available resources from the server |
| `mcp_read_resource(client, uri)` | client: mcp\_client, uri: string | string or list | Read a resource by URI |
| `mcp_list_prompts(client)` | client: mcp\_client | list | List available prompts from the server |
| `mcp_get_prompt(client, name, arguments?)` | client: mcp\_client, name: string, arguments: dict | dict | Get a prompt with optional arguments |
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
