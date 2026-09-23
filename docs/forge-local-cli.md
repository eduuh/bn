# Forge local CLI

`bn forge` is the local bridge from Forge claimable tasks into bn agent dispatch.

## Profile

Each local agent profile lives outside bn config at:

```text
~/.config/wira360/agents/<agent>.json
```

The MVP profile is non-secret configuration only:

```json
{
  "agentName": "Example-agent",
  "claimer": "Example-agent",
  "forgeMcpUrl": "https://api.wira360.com/mcp"
}
```

The nested schema is also accepted for tests/manual profiles:

```json
{
  "claimer": {
    "id": "bn@example-machine",
    "displayName": "Example Forge Agent"
  },
  "forgeMcp": {
    "server": "forge",
    "oauthProfile": "default"
  }
}
```

Required fields:

- `claimer` as a string, or `claimer.id` plus `claimer.displayName` (or equivalent `display` /
  `name`)
- `forgeMcpUrl`, or `forgeMcp.server` / `forgeMcp.endpoint` plus `forgeMcp.oauthProfile` /
  `forgeMcp.oauthSession`

API keys, family keys, bearer tokens, raw OAuth tokens, passwords, authorization headers,
credentials, and other embedded secrets are rejected. Forge access is OAuth MCP only.

## Commands

```text
bn forge list <agent> [--task <id>] [--limit <n>] [--json]
bn forge claim <agent> --task <id> [--lease-seconds <seconds>] [--json]
bn forge dispatch <agent> --task <id> [--repo <repo>] [--agent <harness>] [--name <name>] [--verify] [--json]
```

`list` is side-effect free. It validates the profile and calls Forge MCP `list_bn_tasks` with
`claimable=true`; it never claims, writes notes, or starts dispatch.

`claim` calls Forge MCP `claim_activity` for exactly one task id using the profile's configured
claimer. It does not dispatch local work.

`dispatch` claims first, then starts existing `bn dispatch` with traceable item text in the form
`Forge task <id>: <title>`. If dispatch fails after a successful claim, bn records a blocker in the
current branch note and exits non-zero. The claim is not released unless Forge release semantics are
implemented explicitly.

## MCP transport

`bn forge` uses the Forge server registered in `~/.copilot/mcp-config.json` when it exists. That
registration is written by `setup:forge-agent` and launches `mcp-remote` with the static OAuth
client metadata needed by Forge. If no Copilot registration exists, `bn forge` falls back to:

```text
npx -y mcp-remote@latest <forgeMcp.endpoint|forgeMcpUrl> --protocol auto
```

The stdio proxy uses newline-delimited JSON-RPC frames. The CLI initializes MCP, calls the
documented Forge tools, and parses JSON tool content:

- `list_bn_tasks(claimable=true)`
- `claim_activity(claimer, id, leaseSeconds)`

If the local MCP bridge is missing, cannot authenticate, or returns an unexpected response shape,
`bn forge` exits non-zero with a stable JSON error instead of falling back to API keys.

`/Users/me/projects/worktree/bn/main` is reference-only evidence for this work. Do not edit or
copy code there for the Forge local CLI.
