`std/config.parse_env` parses literal environment files with typed errors and no shell execution.
Coding-agent evaluations and provider probe campaigns share the parser and reject malformed lines
or unfinished quotes. Evaluations restore earlier environment settings when loading fails.
