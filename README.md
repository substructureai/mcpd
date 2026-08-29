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
  "description": "Run a bash command. This is a sandbox environment you can use for anything.",
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
--bind "127.0.0.1:8080"
```


## Connect a harness

**[subs](https://github.com/substructureai/subs)**, in `subs.toml`. Declare the
connection, then give it to an agent:

```toml
[mcp.sandbox]
url = "http://127.0.0.1:8080/mcp"

[agent.coder]
llm = "openrouter"
model = "deepseek/deepseek-v4-flash-0731"
system = "You are a coding agent."
mcp = ["mcp.sandbox"]
```

Hand the connection its token once. It never appears in the file:

```sh
subs auth mcp.sandbox --env MCPD_TOKEN
subs chat coder -c subs.toml
```

**Claude Code**

```sh
claude mcp add --transport http sandbox http://127.0.0.1:8080/mcp \
  --header "Authorization: Bearer $MCPD_TOKEN"
```

**Codex**, in `~/.codex/config.toml`:

```toml
[mcp_servers.sandbox]
url = "http://127.0.0.1:8080/mcp"
bearer_token_env_var = "MCPD_TOKEN"
```

**Cursor**, in `~/.cursor/mcp.json` or `.cursor/mcp.json`:

```json
{
  "mcpServers": {
    "sandbox": {
      "url": "http://127.0.0.1:8080/mcp",
      "headers": { "Authorization": "Bearer ${env:MCPD_TOKEN}" }
    }
  }
}
```

## More examples

See [examples/](examples/) for config that mimics the tools of other popular harnesses.

## License

MIT — see [LICENSE](LICENSE).

