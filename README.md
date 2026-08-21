# mcpd

Built by [substructure.ai](https://substructure.ai)

Pairs well with our [cloud harness](https://github.com/substructureai/substructure)

## Turn your sandbox into an MCP server.

Execute tools inside your sandbox without running your harness inside the sandbox.

## Quick start

Start an MCP server with a single bash tool:

```sh
mcpd --tool '{
  "name": "bash",
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


See [examples/README.md](examples/README.md)
