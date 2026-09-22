# Rift for OpenCode

Give OpenCode disposable workspaces that start from the files you have right now—not only the last Git commit.

Rift snapshots the active checkout with copy-on-write, including staged changes, dirty files, untracked files, and
included ignored files. OpenCode keeps its normal workspace UI, inventory, session flow, and startup commands; this
plugin only replaces the filesystem backend.

## Install

Install Rift and initialize the project once:

```sh
npm install -g rift-snapshot
cd /path/to/project
rift init
```

Then install the plugin from this repository:

```sh
opencode plugin add 'github:anomalyco/rift#dev::path:plugins/opencode'
```

New workspaces created by OpenCode will now use Rift. Existing workspaces keep the backend that created them.

## Exact snapshots

Rift normally leaves out regenerable directories such as `node_modules`, build outputs, and caches. OpenCode can run
its configured `commands.start` in the new workspace to restore what it needs.

Set `copyAll` when you want an exact snapshot instead:

```jsonc
{
  "$schema": "https://opencode.ai/config.json",
  "plugins": [
    {
      "package": "github:anomalyco/rift#dev::path:plugins/opencode",
      "options": { "copyAll": true }
    }
  ]
}
```

Use `database` to point the plugin at a non-default Rift registry.

## Current boundaries

- Rift lifecycle hooks are disabled. OpenCode owns post-create startup through `commands.start`, and worktree failures
  remain atomic from OpenCode's perspective.
- Starting from an explicit Git ref is not supported. Rift snapshots a working directory, not a commit.
- Remove child Rift workspaces before removing their parent.
