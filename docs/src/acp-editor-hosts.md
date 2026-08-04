# Use Harn from ACP editor hosts

Harn can run as a native Agent Client Protocol (ACP) coding agent for editors
that host external ACP agents, including Zed, JetBrains IDEs, Lumide, and other
ACP clients.

## Install

The canonical external-host artifact is the `harn` binary:

```bash
brew tap burin-labs/burin
brew install harn
harn --version
```

Every editor should launch the same stdio command:

```bash
harn serve acp
```

This starts Harn's file-less ACP attach server. The host editor sends
`initialize` and session setup over stdin/stdout after spawning the process; the
launch command does not need a project path or `.harn` file.

## Zed

Until the ACP Registry entry is merged, configure Harn as a custom Zed agent:

```json
{
  "agent_servers": {
    "harn": {
      "type": "custom",
      "command": "harn",
      "args": ["serve", "acp"],
      "env": {}
    }
  }
}
```

After the registry entry lands, install Harn from Zed's agent registry picker.

## JetBrains IDEs

JetBrains reads custom ACP agents from `~/.jetbrains/acp.json`:

```json
{
  "default_mcp_settings": {},
  "agent_servers": {
    "Harn": {
      "command": "harn",
      "args": ["serve", "acp"],
      "env": {}
    }
  }
}
```

Reload the IDE's AI Assistant agent list after editing the file. Once the ACP
Registry entry lands, prefer **Settings -> Tools -> AI Assistant -> Agents ->
Install From ACP Registry** so release metadata updates through the registry.

## Lumide

Lumide 0.14.0 advertises custom ACP-compatible agents. Its public repo confirms
the custom ACP surface, but does not publish a stable JSON config-file schema.
Use the custom ACP agent UI with these launch fields:

| Field | Value |
|---|---|
| Name | `Harn` |
| Command | `harn` |
| Arguments | `serve acp` |
| Environment | Empty, or provider-specific variables such as `ANTHROPIC_API_KEY` |

Prefer the registry picker once Harn is listed there.

## Smoke test

Before relying on an editor integration, test a disposable project:

```bash
mkdir -p /tmp/harn-acp-smoke
cd /tmp/harn-acp-smoke
printf 'status = "before"\n' > smoke.txt
```

Launch Harn from the editor and ask:

```text
Change smoke.txt so the status value is "after", then verify the file.
```

A healthy integration lets Harn read `smoke.txt`, write the edit, and run a
verification command or explain why verification is unavailable. If the editor
cannot find `harn`, replace `command: "harn"` with the absolute path from
`which harn` in the shell environment that launches the editor.

## Registry metadata

The checked-in ACP Registry manifest lives at
`spec/acp-registry/harn/agent.json`. It pins the current release binary
archives, launches `harn serve acp`, and is mirrored into the upstream
`agentclientprotocol/registry` submission. Keep the manifest and Homebrew
formula on the latest published Harn release before refreshing the upstream PR.
