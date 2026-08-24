# mcpd

Built by [substructure.ai](https://substructure.ai)

Pairs well with [subs](https://github.com/substructureai/subs): an agent harness for the cloud.

## Turn your sandbox into an MCP server.

Execute tools inside your sandbox, so your agent harness can run elsewhere.

## Install

```sh
curl -fsSL https://subs.dev/mcpd.sh | bash
```

## Quick start

Start an MCP server with a single bash tool:

```sh
mcpd --tool '{
  "name": "bash",
  "title": "Bash",
  "description": "Run a bash command. Returns combined stdout and stderr.",
  "inputSchema": {
    "type": "object",
    "required": ["command"],
    "properties": {
      "command": { "type": "string", "description": "The command to run." }
    }
  },
  "_meta": {
    "dev.subs/exec": { "argv": ["/bin/bash", "-lc", "{command}"] }
  }
}' \
--no-auth \
--bind "0.0.0.0:8080"
```


See [examples/](examples/) for config that mimics the tools of other popular harnesses.

