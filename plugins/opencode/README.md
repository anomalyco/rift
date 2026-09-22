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

## Agent workspaces

The plugin exposes three Code Mode tools:

- `rift.create` snapshots the current session workspace and moves the session into it by default.
- `rift.list` refreshes and lists Rift workspaces for the current project.
- `rift.remove` removes a Rift workspace and moves a session to an unmanaged project checkout when needed.

Rift runs as a cancellable child process, so lifecycle hooks do not block the OpenCode server. Hook output and failures
are written to the OpenCode server's stderr. Failures after a completed filesystem operation are reconciled with
OpenCode's inventory.

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

Use `executable` to select a non-default Rift binary and `database` to point the plugin at a non-default Rift registry.

Rift lifecycle hooks run by default. This lets projects validate before creation, prepare dependencies or infrastructure
after creation, and clean up external resources around removal. Set `"hooks": false` in the plugin options when
OpenCode's `commands.start` is the only lifecycle behavior you want.

## Current boundaries

- Starting from an explicit Git ref is not supported. Rift snapshots a working directory, not a commit.
- Remove child Rift workspaces before removing their parent.
