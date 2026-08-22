# mcpd

Built by [substructure.ai](https://substructure.ai)

Pairs well with our [cloud harness](https://github.com/substructureai/substructure)

## Turn your sandbox into an MCP server.

Execute tools inside your sandbox without running your harness inside the sandbox.

## Install

```sh
curl -fsSL https://subs.dev/mcpd.sh | bash
```

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

## stdio

Pass `--stdio` and the same tools are served on stdin and stdout instead, one
client per process, for anything that launches its MCP servers itself:

```sh
mcpd --stdio -c examples/claude-code.json --cwd /path/to/checkout
```

```json
{
  "mcpServers": {
    "sandbox": {
      "command": "mcpd",
      "args": ["--stdio", "-c", "/etc/mcpd/tools.json", "--cwd", "/workspace"]
    }
  }
}
```

Nothing is bound and nothing is authenticated: the client is whoever launched
the process, and the pipes reach nobody else. `--bind`, `--mcp-path` and
`--no-auth` are refused alongside `--stdio` rather than quietly ignored; a
config file that carries a `bind` for the HTTP server is simply not used for it.
Logs always go to stderr, so stdout carries nothing but the protocol.

See [examples/README.md](examples/README.md)
