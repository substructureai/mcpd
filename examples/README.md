# Example configurations

Every key here is a command-line flag spelled the same way, so anything in a
file can also be passed as `--flag`, and a flag always wins over the file.
Both `.toml` and `.json` are accepted; the extension decides which.

JSON suits these files better. A tool definition is an MCP tool definition, so
in a JSON config the entries under `tool` are exactly what `--tool` takes, and
you can move one between the two by copying it.

| File | What it shows |
| --- | --- |
| `codex.json` | Codex's toolset: `shell` and `apply_patch` |
| `claude-code.json` | Claude Code's toolset: `Bash`, `Read`, `Write`, `Edit`, `Glob`, `Grep` |
| `readonly.json` | No shell at all — only read-only commands |
| `minimal.toml` | The smallest useful server, and the TOML form |

## Running one

The token is read from `MCPD_TOKEN` and never from a flag, so it stays out of
`ps`, out of shell history, and out of anything you paste into a chat.

```sh
export MCPD_TOKEN=$(openssl rand -hex 32)
mcpd -c examples/claude-code.json --cwd /path/to/checkout
```

For local work where a token is more friction than protection, `--no-auth`
serves every caller. It has to be typed at launch; a config file cannot set it.

## Where these came from

`codex.json` follows `openai/codex` (Apache-2.0): the `shell` tool takes its
`command` as an argv array and is executed directly rather than through a
shell, and the `apply_patch` description is adapted from that repository's
patch-format documentation. The instructions are condensed from its system
prompt. `apply_patch` expects the `apply_patch` binary on `PATH`; without it
the tool reports a spawn failure and the other tools still work.

`claude-code.json` follows Claude Code's tool names and parameter names —
`file_path`, `old_string`/`new_string`/`replace_all`, `offset`/`limit`,
`pattern`/`glob`/`-C`. The implementations are ordinary commands: `Read` is
`awk` producing line-numbered output, `Write` is `dd`, `Edit` is a `perl`
program that refuses to write unless `old_string` matches exactly once, and
`Glob`/`Grep` are `rg`.

## Where they deviate, and why

Substitution has no conditionals. A placeholder becomes a value; it cannot add
or remove a flag. Anything whose shape is "a boolean that changes the command
line" therefore has no direct expression:

- `Bash` has no per-call `timeout`. A tool's timeout lives in its definition,
  under `timeoutMs`, and applies to every call.
- `Grep` has no `-i`, `-n`, `output_mode`, `head_limit` or `multiline`.
  `--smart-case` and `--line-number` are always on, which covers the common
  case; `maxOutputBytes` bounds the output instead of `head_limit`.
- Parameters that are only ever values do carry across, including hyphenated
  names like `-C`, which substitute normally.

The workaround for a genuinely optional flag is an array parameter: an array
spliced into a bare `{param}` element becomes several argv elements, so a
`flags` parameter can carry whatever the model needs. That is how `search`
in the other examples accepts several paths.

## Writing your own

A tool is an MCP tool definition with one extra key, `_meta."dev.subs/exec"`,
holding `argv` plus optional `cwd`, `stdin`, `timeoutMs` and `maxOutputBytes`.

`{param}` placeholders are substituted element-wise into `argv`, never into a
joined string, so nothing a model writes reaches a shell it wasn't given. A
placeholder only means anything if the schema declares that property, so
`awk '{print $1}'` survives untouched.

Anything wrong with a definition — a duplicate name, an empty `argv`, a
misspelled `timeout` for `timeoutMs`, a substituted parameter that is neither
required nor defaulted — stops the daemon at startup rather than failing on the
call an agent makes twenty minutes later.

Give array parameters `"minItems": 1` when they supply the command itself, as
`codex.json` does: an empty array would otherwise leave nothing to run, and the
call is rejected rather than executed.

## A note on where this runs

`mcpd` will run whatever the tools you declare can run, and it does not sandbox
anything: the machine it sits on is the boundary. Run it inside a container or
a VM you are willing to hand to a model, and let the tool list be the only
thing that decides what is reachable.
