# Rift for OpenCode

Use Rift workspaces as the worktree backend in OpenCode V2.

```sh
npm install -g rift-snapshot
rift init
opencode plugin add 'github:anomalyco/rift#v0.0.12::path:plugins/opencode'
```

OpenCode's workspace UI then creates Rift workspaces, and agents get `rift.create`, `rift.list`, and `rift.remove`.
Move sessions with OpenCode's `opencode.session_move` tool.

## Options

```jsonc
{
  "plugins": [
    {
      "package": "github:anomalyco/rift#v0.0.12::path:plugins/opencode",
      "options": { "copyAll": false, "hooks": true, "executable": "rift", "database": "/path/to/rift.sqlite" }
    }
  ]
}
```

- `copyAll`: exact snapshots instead of filtered ones.
- `hooks`: run `.rift.toml` lifecycle hooks. Hook output goes to the OpenCode server's stderr.
- `executable`: Rift binary to run.
- `database`: non-default Rift registry.

## Limits

- Starting refs are not supported; Rift snapshots a working directory, not a commit.
- Remove a workspace's child Rifts before removing it.
- `rift.remove` refuses to remove the workspace the current session is in.
