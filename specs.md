# Rift Specs

## Requirement

`rift` must be cross-platform as far as practical. Core semantics should work across macOS, Linux, and Windows. On Linux, managed workspaces use either btrfs subvolumes for instantaneous writable snapshots or native per-file reflinks for copy-on-write tree cloning.

## API

### `init`

```ts
init(input: {
  at: AbsolutePath
}): void
```

`init` prepares and registers an original workspace for Rift.

- On Linux, `at` must be on btrfs or a filesystem with native reflink support; on other supported systems, initialization registers the workspace without filesystem conversion.
- If `at` is already a btrfs subvolume, register it without replacing it.
- If `at` is an ordinary btrfs directory, reflink-import it once into a staged btrfs subvolume and atomically replace the original directory at its existing path.
- On other Linux filesystems, verify native reflink support and register `at` without replacing it.
- The original directory is retained under an internal temporary path only while it is needed for rollback and is removed before a successful `init` returns.
- The core operation initializes exactly `at` and does not search parent directories.
- The CLI defaults `at` to the current working directory; by default it selects the nearest existing managed ancestor or nearest Git root, prints the selected path, and then invokes core `init` with that exact path. `--here` opts into selecting exactly the supplied path.
- Calling `init` inside an already initialized workspace reports the existing root; if that root's `.rift` marker was deleted, `init` restores the marker using its existing registry identity.
- After a conversion it tells the caller to re-enter the original path.

### `create`

```ts
create(input: {
  from: AbsolutePath
  name?: string
  into?: AbsolutePath
  copyAll?: boolean
  hooks?: boolean
}): AbsolutePath
```

Default behavior:

- Source is `from`.
- `name` defaults to a random adjective-noun directory name independent of the rift ULID.
- `into` defaults to the managed rift directory.
- Copy the workspace while excluding known heavyweight regenerable dependency, build, and cache artifacts.
- Preserve manifests, lockfiles, dirty files, staged files, untracked files, and ignored files that are not part of the built-in excluded artifact set.
- `copyAll` opts into exact copying, including dependency and build artifacts.
- `hooks` defaults to true and runs `.rift.toml` precreate hooks before copying and postcreate hooks after workspace creation, Git preparation, and registry insertion. `hooks: false` skips config loading and hook execution.
- Detach `HEAD` in the new workspace.
- Return the path of the new workspace.

Default excluded artifacts are matched at any depth and include `node_modules`, `.pnpm-store`, `.yarn/cache`, `.yarn/unplugged`, `.yarn/install-state.gz`, `.yarn/build-state.yml`, `target`, `.venv`, `venv`, `.tox`, `.nox`, `__pycache__`, `.pytest_cache`, `.mypy_cache`, `.ruff_cache`, `.next`, `.nuxt`, `.svelte-kit`, `.turbo`, `.vite`, `.parcel-cache`, `.cache`, `dist`, `build`, and `coverage`.

`.rift.toml` supports four v1 lifecycle hooks with the same shape:

```toml
version = 1

[[hooks.precreate]]
run = "pnpm run check"

[[hooks.postcreate]]
run = "pnpm install --frozen-lockfile"

[[hooks.preremove]]
run = "pnpm run cleanup"

[[hooks.postremove]]
run = "echo removed"
```

Hooks run sequentially with inherited stdio and environment plus `RIFT_SOURCE`, `RIFT_DESTINATION`, `RIFT_ID`, and `RIFT_PARENT_ID`. Precreate runs in the source workspace and postcreate runs in the destination. The first failing command stops later hooks. A precreate failure prevents copying; after a postcreate failure, the created workspace remains registered and on disk.

On btrfs, `from` must already be a subvolume. If it is an ordinary directory, fail and instruct the user to run `rift init` first. On other reflink-capable Linux filesystems, clone the directory tree with native per-file reflinks.

If `from` is already managed by Rift, create copies that exact directory. Do not resolve back to an earlier workspace. Metadata should record the immediate source rift as its parent.

Default storage is a hidden sibling directory of the original registered workspace:

```text
/projects/app/                         original workspace
/projects/.rifts/app/task-a/           created rift
/projects/.rifts/app/task-b/           created rift
```

- Created rifts must not be stored inside the workspace being copied, because an exact copy would recursively contain existing rifts.
- `from` resolves upward to the nearest `.rift` marker and must belong to an initialized workspace; if no marker is found, instruct the user to run `rift init` in the root folder.
- The original registered workspace's sibling `.rifts/<workspace-name>/` directory becomes the default destination directory.
- If `from` is already managed, descendants use the default destination directory associated with the original workspace rather than nesting storage beside each descendant.
- If `into` is provided, use it instead of the default destination directory.
- If the original workspace is itself a filesystem mount root, its sibling default destination may not support copy-on-write with it; provide `into` on the same filesystem in that case.

### `remove`

```ts
remove(input: {
  at: AbsolutePath
  all?: boolean
  hooks?: boolean
}): void
```

`remove` logically deletes a created rift subtree by moving it into Rift-owned trash, or unregisters a registered source root while preserving its directory.

- If `at` identifies a registered source root, preserve its directory, delete its `.rift` marker, move each existing registered descendant into trash, tolerate descendants already absent from disk, and delete its active registry tree.
- The CLI requires `-f` or `--force` when `remove` would unregister a registered source root; this confirmation is not part of the core or FFI operation.
- The CLI exposes the descendant-preserving mode as `rift remove --children`; the core and FFI input field remains `all`.
- If `at` identifies a created rift, move its full descendant subtree into trash.
- When `all` is true, preserve `at` and delete every managed descendant. In this mode `at` may be the registered source root.
- `hooks` defaults to true. Preremove runs in `at` before filesystem or registry changes. Postremove runs after successful removal, from the trash directory when `at` was moved and from `at` when it was preserved. A preremove failure prevents removal; a postremove failure reports an error without rolling back the completed removal.
- Resolve all descendants through `parent_id` and move their directories deepest-first.
- Verify each existing directory's `.rift` marker before deleting it.
- Refuse removal if any descendant path is missing, because the registered active tree no longer matches the filesystem.
- Move each removed rift from `<storage-parent>/<name>` to `<storage-parent>/.trash/<id>-<name>` so custom `into` storage remains on the same filesystem.
- After successful filesystem moves, delete the active tree records and insert trash records for garbage collection.

### `list`

```ts
list(input: {
  of: AbsolutePath
}): AbsolutePath[]
```

`list` returns the direct active managed rifts created from `of`.

### `ancestors`

```ts
ancestors(input: {
  of: AbsolutePath
}): AbsolutePath[]
```

`ancestors` returns the managed ancestry of `of`, ordered from its immediate parent to the root workspace.

### `gc`

```ts
gc(): AbsolutePath[]
```

`gc` physically deletes rifts previously moved into Rift-owned trash and returns deleted trash paths for CLI output.

- On btrfs, attempt immediate subvolume deletion first.
- If standard mount permissions deny deletion of a populated subvolume, delete its contents and remove the now-empty subvolume with ordinary directory removal.
- On reflink-backed Linux filesystems, recursively remove the reflinked directory tree.
- Delete each trash registry record after its filesystem directory is successfully removed.
- Delete active registry records whose filesystem directories were removed outside Rift only when no existing recorded descendant would be orphaned, and include pruned missing paths in the result.

## Metadata

Metadata is stored in a central SQLite database in the platform-appropriate user data directory.

SQLite is not overkill: multiple processes and agents may create, inspect, or remove rifts concurrently. It provides cross-platform transactions and locking without building a safe JSON registry protocol.

Start with one table:

```sql
CREATE TABLE rift (
  id TEXT PRIMARY KEY,
  parent_id TEXT REFERENCES rift(id) ON DELETE CASCADE,
  path TEXT NOT NULL UNIQUE,
  created_at INTEGER NOT NULL
);

CREATE INDEX rift_parent_id_idx ON rift(parent_id);

CREATE TABLE trash (
  id TEXT PRIMARY KEY,
  path TEXT NOT NULL UNIQUE,
  removed_at INTEGER NOT NULL
);
```

- Every managed rift has a stable generated `id`.
- `id` is a ULID generated when the workspace is first registered or created.
- `id` is stored in the central database and in a `.rift` marker file at the root of the workspace.
- `.rift` contains the rift ULID and allows a workspace directory to be verified against the database.
- When a managed workspace is copied, the copied `.rift` marker is replaced with the new workspace's ULID.
- The original registered workspace has `parent_id = NULL`.
- A created rift has `parent_id` set to the source rift `id`.
- `path` is its current location, not its identity.
- Provenance is a rooted tree. Descendants of any rift can be listed through recursive queries over `parent_id`.
- `remove` moves a whole active subtree into trash, so no surviving active record depends on deleted ancestry.

## Git Integration

Git support is an integration for directories that contain repositories; it does not define the core Rift model.

When registering or creating from a Git repository:

- Add `/.rift` to `.git/info/exclude` so the identity marker does not appear in local Git status.
- Preserve staged, unstaged, untracked, ignored, and cached state for copied paths.
- If `HEAD` resolves to a commit, detach `HEAD` in the created destination at that same commit.
- Preserve the copied index and working tree state while detaching.
- If the repository has no commits yet, leave its unborn branch state unchanged because there is no commit to detach to.

Refuse creation from a Git repository when:

- It is a linked Git worktree whose `.git` is not an independent directory.
- A merge, rebase, cherry-pick, revert, or bisect is in progress.
- Git lock or inconsistent index state makes an exact safe copy unclear.

The tool does not create branches, commit changes, or otherwise replace normal Git commands.

## Copy Strategies

Copying is implemented behind a `Strategy` interface so platform-specific copy-on-write backends can be added independently. Each strategy owns initialization, snapshot creation, and removal behavior for its filesystem.

- The `BtrfsStrategy` production strategy on Linux uses writable btrfs subvolume snapshots.
- The `BtrfsStrategy` performs native per-file reflink imports when `init` converts an existing ordinary workspace into a subvolume and when filtered `create` materializes only included paths. Exact `create` uses writable btrfs snapshots.
- The `LinuxReflinkStrategy` production strategy on Linux verifies native reflink support during `init` and uses native per-file reflinks during `create` without spawning an external copy command. XFS uses this path, as do other Linux filesystems when their `FICLONE` support succeeds.
- The `ApfsStrategy` production strategy on macOS uses APFS `clonefile` directory cloning for exact copies and per-entry cloning for filtered copies.
- If no implemented copy-on-write strategy succeeds, `create` fails.
- Full byte copying is not implemented as a fallback.
- Future strategies may add Windows copy-on-write support without changing the API.

## Packaging

The project ships four interfaces backed by the same implementation and metadata model:

1. Native library containing the core API and implementation.
2. CLI package providing the `rift` executable.
3. Bun FFI package for use from Bun applications.
4. Node FFI package for use from Node.js applications.

The CLI and language bindings should remain thin and expose the same API semantics as the native library.

The npm launcher package temporarily publishes as `rift-snapshot` and bundles prebuilt CLI binaries and FFI shared libraries for every supported target under `prebuilds/<platform>-<arch>/`. It must not require install lifecycle scripts; its CLI shim resolves the bundled executable at runtime, and conditional exports make `import "rift-snapshot"` select the Bun or experimental Node FFI binding automatically. When the `rift` npm name is available, only the launcher package name changes.

For CLI ergonomics, the primary workspace path for `rift init`, `rift create`, `rift remove`, `rift list`, and `rift ancestors` defaults to the current working directory when it is omitted. Workspace operations locate their root by searching upward for its `.rift` marker. The CLI applies similar selection before calling exact-path core `init`, unless `rift init --here` is explicitly requested.

The CLI may provide opt-in Bash, Zsh, and Nushell integration through `rift shell-init <shell>`. The resulting shell function delegates filesystem and registry operations to the executable, then changes the caller's working directory after `init`, `create`, or removal of the current rift. This shell behavior is not part of the native library or FFI APIs.
