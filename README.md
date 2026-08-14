> **Warning: Experimental repository**
>
> This repository is experimental and is not ready for use. We are exploring a variety of ideas here, and behavior, interfaces, and implementation details may change without notice.

rift: better alternative to git worktrees

- copy on write (saves space)
- instant (< 0.1s on 10gb folder)
- fast cli
- use as FFI lib with bun or node

mac and linux with btrfs or native reflinks for now
more support soon

## Install

```bash
npm install -g rift-snapshot
# or
bun add -g rift-snapshot
```

Release archives are available from [GitHub Releases](https://github.com/anomalyco/rift/releases/latest).

## Platforms

| Platform          | Backend                             | Behavior                                                           |
| ----------------- | ----------------------------------- | ------------------------------------------------------------------ |
| Linux x64         | Writable btrfs snapshots            | `rift init` converts an ordinary directory into a btrfs subvolume. |
| Linux x64         | Native per-file reflinks            | `rift init` verifies reflink support and registers the directory.  |
| macOS arm64 / x64 | APFS `clonefile`                    | `rift init` registers the source directory.                        |
| Windows x64       | None                                | The package is published; workspace creation is not implemented.   |

## CLI

### Initialize

```bash
cd ~/code/app
rift init
```

`rift init` selects an existing Rift root above the current directory, or the nearest Git root when no Rift root exists. Use `--here` to initialize exactly the selected directory.

On Linux, first initialization of an ordinary btrfs directory performs a reflink import into a new btrfs subvolume and swaps it into the same path. On other Linux filesystems, initialization verifies native reflink support and registers the directory in place. This includes XFS and other filesystems when their `FICLONE` support succeeds. If the selected root is registered already, no conversion occurs. If its `.rift` marker is missing, `rift init` restores it and completes any required setup.

### Create

```bash
rift create
rift create --name parser-fix
rift create --into /fast/rifts
rift create --copy-all
rift create --no-hooks
```

`rift create` searches upward for `.rift`, copies that managed workspace, records the immediate parent, and prints the new workspace path to stdout.

By default, creation omits heavyweight regenerable dependency and build artifacts such as `node_modules`, `target`, virtualenvs, framework caches, `dist`, `build`, and `coverage`. Manifests and lockfiles are preserved. Use `--copy-all` to keep the previous exact-copy behavior.

On btrfs, exact copies use writable subvolume snapshots and filtered copies use a reflink import into a new subvolume. On other reflink-capable Linux filesystems, Rift reflink-clones the selected directory tree. On macOS, exact copies use APFS `clonefile`, and filtered copies clone included entries.

When the workspace is a Git repository, the new workspace has detached `HEAD` and retains index and working-tree state.

If the source contains `.rift.toml`, `rift create` runs configured precreate hooks before copying and postcreate hooks after the workspace is created, registered, and prepared. Use `--no-hooks` to skip them.

```toml
version = 1

[[hooks.precreate]]
run = "pnpm run check"

[[hooks.postcreate]]
run = "pnpm install --frozen-lockfile"

[[hooks.postcreate]]
run = "pnpm run codegen"
```

Precreate commands run in the source workspace; postcreate commands run in the new workspace. A precreate failure prevents creation. If a postcreate hook fails, the workspace remains registered and `rift create` exits with an error.

### List And Ancestors

```bash
rift list
rift ancestors
```

`list` prints direct active child workspaces. `ancestors` prints parent workspaces, nearest first.

### Remove And Garbage Collection

```bash
rift remove                         # trash the current created rift subtree
rift remove -f ~/code/app           # unregister a source root
rift remove --children ~/code/app   # trash descendants, preserve the selected workspace
rift remove --no-hooks ~/code/app/task
rift gc                             # physically delete trash and prune missing entries
```

Removing a created rift moves its active subtree into adjacent `.trash` storage. `rift gc` deletes that storage later.

Removing a source root requires `-f` in the CLI. The source directory remains on disk. Its `.rift` marker is removed. Existing registered descendants are moved into trash. Missing descendants are removed from the registry.

`preremove` hooks run in the selected workspace before removal. `postremove` hooks run after removal, from the moved trash directory when the selected workspace was trashed and from the selected workspace when it was preserved. Use `--no-hooks` to skip remove hooks.

```toml
[[hooks.preremove]]
run = "pnpm run cleanup"

[[hooks.postremove]]
run = "echo removed $RIFT_SOURCE"
```

### Shell Integration

```bash
eval "$(rift shell-init zsh)" # or bash
```

```nushell
rift shell-init nushell | save -f (($nu.user-autoload-dirs | first) | path join "rift.nu")
```

The shell wrapper changes directory after `init` conversion, `create`, or removal of the current created rift.

## Storage

Each managed workspace has a `.rift` marker containing its identifier. An SQLite registry stores paths, parent identifiers, and trash entries.

Default created-workspace storage is adjacent to the registered source root:

```text
~/code/app/                         source workspace
~/code/.rifts/app/parser-fix/       created workspace
~/code/.rifts/app/.trash/            removed workspace storage
```

## JavaScript API

The package selects a Bun or Node FFI binding through conditional exports.

```ts
import { create, list, remove, gc } from "rift-snapshot";

const workspace = create({ from: process.cwd(), name: "schema-work" });
console.log(list({ of: process.cwd() }));
remove({ at: workspace });
gc();
```

### Node.js

The Node binding requires the experimental FFI API in Node.js 26.1 or later:

```bash
node --experimental-ffi app.mjs
```

With Node's permission model, also pass `--allow-ffi`.

### Functions

```ts
init(options?: { at?: string; database?: string }): null
create(options?: { from?: string; name?: string; into?: string; copyAll?: boolean; hooks?: boolean; database?: string }): string
remove(options?: { at?: string; all?: false; hooks?: boolean; database?: string }): void
remove(options: { at?: string; all: true; hooks?: boolean; database?: string }): string[]
list(options?: { of?: string; database?: string }): string[]
ancestors(options?: { of?: string; database?: string }): string[]
gc(options?: { database?: string }): string[]
```

The JavaScript `init` function initializes exactly `at`; Git-root selection and `--here` are CLI behavior.

Operation failures throw `RiftError` with a `code` and, when relevant, `path`.

## Development

```bash
cargo test --workspace --locked
./scripts/install.sh
```

`scripts/install.sh` installs an optimized CLI binary to `${CARGO_HOME:-$HOME/.cargo}/bin/rift`.

### Benchmark

Benchmark a single real `rift create` operation against a directory:

```bash
cargo bench --bench create -- /path/to/linux
```

The benchmark initializes the supplied directory before timing, times only creation of the new rift, and then removes the created workspace outside the measured interval. On first use, initialization of an ordinary Linux btrfs directory converts it into a subvolume before measurement. The benchmark uses the production filesystem strategy, so results measure APFS cloning on macOS, btrfs snapshots on btrfs, and per-file reflinks on reflink-capable Linux filesystems.

Establish a baseline by measuring multiple independent rift creations and writing an aggregate machine-readable result file. Keep results outside the source workspace so they do not alter future measurements:

```bash
cargo bench --bench create -- /path/to/linux --samples 10 --output /path/to/results/baseline.json
```

The JSON result includes each timing sample and the median, minimum, and maximum elapsed time. A future experiment loop can run the same command in candidate workspaces and compare their median results to this baseline.

Compare multiple candidate `rift` code workspaces that contain this benchmark target:

```bash
cargo bench --bench compare -- /path/to/linux \
  --candidate /path/to/rift-baseline \
  --candidate /path/to/rift-candidate-a \
  --candidate /path/to/rift-candidate-b \
  --samples 10 \
  --output /path/to/results/create-run-01
```

The comparison runner invokes each candidate's optimized `create` benchmark against the same workload, writes `candidate-01.json`, `candidate-02.json`, and so on, then writes `summary.json` with candidates ranked by median creation time. Include the unchanged workspace as one candidate when you need a baseline in the ranking.

## License

MIT
