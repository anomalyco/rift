# Rift for OpenCode

Use Rift workspaces as the worktree backend in OpenCode V2.

```sh
npm install -g rift-snapshot@0.0.13
rift init
opencode plugin add 'github:anomalyco/rift#v0.0.13::path:plugins/opencode'
```

OpenCode's workspace UI then creates Rift workspaces, and agents get `rift.create`, `rift.list`, and `rift.remove`.
Move sessions with OpenCode's `opencode.session_move` tool.

## Options

```jsonc
{
  "plugins": [
    {
      "package": "github:anomalyco/rift#v0.0.13::path:plugins/opencode",
      "options": { "copyAll": false, "hooks": true, "executable": "rift", "database": "/path/to/rift.sqlite" }
    }
  ]
}
```

- `copyAll`: exact snapshots instead of filtered ones.
- `hooks`: run `.rift.toml` lifecycle hooks. Hook output is not streamed; failures include its last 8 KiB.
- `executable`: Rift binary to run.
- `database`: non-default Rift registry.

## Limits

- Starting refs are not supported; Rift snapshots a working directory, not a commit.
- Remove a workspace's child Rifts before removing it.
- `rift.remove` refuses to remove the workspace the current session is in.
- On Windows, workspaces need a ReFS volume such as a [Dev Drive](https://learn.microsoft.com/windows/dev-drive/).
- On Windows, removing a workspace fails while OpenCode still has terminals, shells, or MCP servers running in it,
  because OpenCode does not stop them before it calls the worktree strategy's `remove`.
