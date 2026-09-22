# Rift

Instant copy-on-write workspaces for coding agents and parallel work.

> Early software. Behavior, interfaces, and storage details may still change.

## Install

```bash
npm install -g rift-snapshot
# or
bun add -g rift-snapshot
```

Release archives are available from [GitHub Releases](https://github.com/anomalyco/rift/releases/latest).

## Quick Start

```bash
cd ~/code/app
rift init
rift create --name parser-fix
rift list
rift remove ~/code/.rifts/app/parser-fix
rift gc
```

`rift init` registers the project root. `rift create` snapshots the current workspace and prints the new path.
`rift remove` moves a workspace to trash, and `rift gc` deletes trashed workspaces.

Add shell integration to `cd` automatically after `init`, `create`, and `remove`:

```bash
eval "$(rift shell-init zsh)" # or bash
```

```nushell
rift shell-init nushell | save -f (($nu.user-autoload-dirs | first) | path join "rift.nu")
```

## Lifecycle Hooks

Add `.rift.toml` to a workspace to run commands around creation and removal:

```toml
version = 1

[[hooks.precreate]]
run = "pnpm run check"

[[hooks.postcreate]]
run = "pnpm install --frozen-lockfile"

[[hooks.postcreate]]
run = "docker compose -p rift-$RIFT_ID up -d"

[[hooks.preremove]]
run = "pnpm run cleanup"

[[hooks.postremove]]
run = "docker compose -p rift-$RIFT_ID down -v"
```

| Hook         | Runs in                     | On failure                                   |
| ------------ | --------------------------- | -------------------------------------------- |
| `precreate`  | source workspace            | nothing is created                           |
| `postcreate` | new workspace               | workspace stays registered; command fails     |
| `preremove`  | selected workspace          | nothing is removed                           |
| `postremove` | trashed or preserved path   | removal stays complete; command fails         |

Hooks receive `RIFT_SOURCE`, `RIFT_DESTINATION`, `RIFT_ID`, and `RIFT_PARENT_ID`. Hook output goes to stderr so
workspace paths on stdout stay machine-readable. Use `--no-hooks` to skip hooks.

## Agents and OpenCode

Each agent can get an isolated copy of your real working state instead of sharing your checkout or starting from a
clean commit.

Use Rift as the worktree backend in OpenCode V2:

```sh
opencode plugin add 'github:anomalyco/rift#v0.0.12::path:plugins/opencode'
```

Or configure it manually:

```jsonc
{
  "$schema": "https://opencode.ai/config.json",
  "plugins": ["github:anomalyco/rift#v0.0.12::path:plugins/opencode"]
}
```

After `rift init` in the project root, OpenCode's workspace UI creates Rift workspaces, agents get `rift.create`,
`rift.list`, and `rift.remove` Code Mode tools, and OpenCode's session tools move sessions between workspaces. See
[`plugins/opencode`](plugins/opencode) for options and limitations.

## CLI

### `rift init`

```bash
rift init
rift init --here
```

Selects an existing Rift root above the current directory, or the nearest Git root when no Rift root exists. `--here`
initializes exactly the selected directory. If a registered root lost its `.rift` marker, `init` restores it.

### `rift create`

```bash
rift create
rift create --name parser-fix
rift create --into /fast/rifts
rift create --copy-all
rift create --no-hooks
```

Copies the nearest managed workspace, records it as the parent, and prints the new workspace path. Filtered copies
omit regenerable artifacts such as `node_modules`, `target`, virtualenvs, framework caches, `dist`, `build`, and
`coverage`; manifests and lockfiles are kept. `--copy-all` makes an exact copy.

Git repositories are copied with detached `HEAD`, preserving index and working-tree state. Linked worktrees and
repositories with in-progress merges, rebases, cherry-picks, reverts, bisects, or lock files are rejected.

### `rift list` and `rift ancestors`

```bash
rift list
rift ancestors
```

`list` prints direct child workspaces. `ancestors` prints parent workspaces, nearest first.

### `rift remove` and `rift gc`

```bash
rift remove                         # trash the current created rift subtree
rift remove -f ~/code/app           # unregister a source root
rift remove --children ~/code/app   # trash descendants, keep the selected workspace
rift remove --no-hooks ~/code/app/task
rift gc                             # delete trash and prune missing entries
```

Removing a created workspace moves its subtree into adjacent `.trash` storage. Unregistering a root requires `-f`,
keeps the source directory, removes its `.rift` marker, and trashes registered descendants.

## How It Works

| Platform          | Backend                  | Notes                                                  |
| ----------------- | ------------------------ | ------------------------------------------------------ |
| Linux x64         | btrfs snapshots          | `rift init` converts a directory into a subvolume       |
| Linux x64         | Native per-file reflinks | XFS and other filesystems with working `FICLONE`        |
| macOS arm64 / x64 | APFS `clonefile`         | Requires an APFS volume                                 |
| Windows x64       | None                     | Package is published; workspace creation is unsupported |

Each managed workspace has a `.rift` marker containing its ID. A SQLite registry stores paths, parents, and trash
entries. Default storage is adjacent to the source root:

```text
~/code/app/                         source workspace
~/code/.rifts/app/parser-fix/       created workspace
~/code/.rifts/app/.trash/           removed workspace storage
```

Workspaces never overlap, concurrent creates never delete each other's destinations, and removal is a trash operation
until `rift gc` runs.

## JavaScript API

The package selects a Bun or Node FFI binding through conditional exports.

```ts
import { create, list, remove, gc } from "rift-snapshot";

const workspace = create({ from: process.cwd(), name: "schema-work" });
console.log(list({ of: process.cwd() }));
remove({ at: workspace });
gc();
```

```ts
init(options?: { at?: string; database?: string }): null
create(options?: { from?: string; name?: string; into?: string; copyAll?: boolean; hooks?: boolean; database?: string }): string
remove(options?: { at?: string; all?: false; hooks?: boolean; database?: string }): void
remove(options: { at?: string; all: true; hooks?: boolean; database?: string }): string[]
list(options?: { of?: string; database?: string }): string[]
ancestors(options?: { of?: string; database?: string }): string[]
gc(options?: { database?: string }): string[]
```

Node requires the experimental FFI API in Node.js 26.1 or later (`node --experimental-ffi`, plus `--allow-ffi` under
the permission model). `init` initializes exactly `at`; Git-root selection is CLI behavior. Calls are synchronous, so
lifecycle hooks block the caller. Failures throw `RiftError` with `code`, and when relevant `path`, `hook`, and
`committed`.

## Development

```bash
cargo test --workspace --locked
./scripts/install.sh
```

`scripts/install.sh` installs an optimized CLI binary to `${CARGO_HOME:-$HOME/.cargo}/bin/rift`.

Benchmark a real `rift create` against a directory, or compare candidate Rift checkouts:

```bash
cargo bench --bench create -- /path/to/linux --samples 10 --output /path/to/results/baseline.json
cargo bench --bench compare -- /path/to/linux --candidate /path/to/rift-a --candidate /path/to/rift-b --samples 10 --output /path/to/results/run-01
```

Results include per-sample timings plus median, minimum, and maximum; `compare` ranks candidates by median.

## License

MIT
