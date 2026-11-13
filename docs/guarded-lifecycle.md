# Guarded native lifecycle

Use this API to review exact canonical paths, move an exact registered closure to adjacent trash, and restore it. The API is in `rift::guarded`. The CLI requires an explicit database. It does not use the default registry.

## Ownership gate

Before an apply, recovery, finalization, or rollback, stop all old native callers and all outside tools that can change the database or selected directories. Keep them stopped until the active journal is resolved or finalized. Release 0.0.12 does not use the new native lock or journal fence. Schema compatibility does not give old callers safe concurrent filesystem access.

Candidate `Manager` callers share an exclusive lock on the registry parent directory. They refuse to open a registry with an active guarded journal. Registries in the same directory are serialized. The lock has a short bounded wait. SQLite `BEGIN IMMEDIATE` excludes competing SQL writers during filesystem and registry transitions. These controls do not stop an outside process from changing files. Guards detect such changes and refuse to overwrite them. Do not treat detection as exclusive ownership.

## Commands

```sh
rift guarded provenance
rift --database /tmp/synthetic/registry.sqlite guarded preflight request.json > plan.json
rift --database /tmp/synthetic/registry.sqlite guarded apply plan.json
rift --database /tmp/synthetic/registry.sqlite guarded recover
rift --database /tmp/synthetic/registry.sqlite guarded finalize --journal-hash REVIEWED_SHA256
rift --database /tmp/synthetic/registry.sqlite guarded rollback --journal-hash REVIEWED_SHA256
rift --database /tmp/synthetic/registry.sqlite guarded rollback --history EXACT_HISTORY_PATH --journal-hash REVIEWED_SHA256
```

Read and review the complete plan before apply. Read and hash the current journal before finalize or rollback. A hash refers to its exact current bytes. Protect plans and journals from edits. They are review artifacts, not signed authorization tokens.

A reconciliation request changes only these row paths:

```json
{
  "operation": "reconcile",
  "paths": [{"id": "SYNTHETIC-ROOT", "canonical": "/tmp/synthetic/current-source"}]
}
```

The old stored path can be a symlink to the reviewed directory. The directory, marker, Rift ID, parent links, timestamps, rowid, old alias, and unrelated rows stay intact. Reconciliation does not rename a directory or change an alias. A target must be the exact actual canonical path. Duplicate targets and paths owned by another row are rejected.

An exact root trash request has this form:

```json
{
  "operation": "trash",
  "kind": "root",
  "id": "SYNTHETIC-ROOT",
  "closure": ["SYNTHETIC-ROOT", "SYNTHETIC-CHILD"],
  "destinations": [
    {"id": "SYNTHETIC-ROOT", "trash": "/tmp/synthetic/.rift-trash-SYNTHETIC-ROOT"},
    {"id": "SYNTHETIC-CHILD", "trash": "/tmp/synthetic/clones/.rift-trash-SYNTHETIC-CHILD"}
  ],
  "hooks": true
}
```

`root` requires no parent. `leaf` requires a parent. A leaf with children requires the exact explicit child closure. The closure includes the selected ID and every registered descendant. The CLI does not select a wildcard subtree. Each destination must be an absent adjacent `.rift-trash-ID` path. Reconcile stored aliases in a separate reviewed operation before trash. A root is moved with its marker; it is never only unregistered.

Physically nested selected directories are rejected. Unrelated active directories and old trash objects inside a selected directory are rejected. The registry must be outside the selected directories. No source Git operation, directory deletion, force option, old root unregister, marker bypass, or GC is used.

## Preflight and plan

`preflight(database, request)` returns a versioned `Plan`. It contains the build provenance, exact database identity and DB/WAL/SHM hashes, complete active rows with rowids and all stored fields, old trash rows, schema, auxiliary-table fingerprints, database header versions, and selected filesystem guards. Guards include stored and canonical paths, directory identity, ancestor and symlink identities, marker ID/hash, and the full directory tree hash.

Dry preflight reads source files and opens SQLite only on a temporary byte copy of the DB, live WAL, and SHM. It compares source bytes again after the copy and filesystem review. It does not open a writable `Manager`, create source locks or directories, change schema, run hooks, or write markers. It does not use `immutable=1`. An unstable copy fails. Temporary inspection files are outside the source registry and selected directories.

`apply(plan)` requires the same native build provenance and exact snapshot. It rechecks the full logical state before opening source SQLite. Unknown auxiliary table data and old trash rows are preserved. Extra active-row fields are rejected to prevent lossy history. The current release fields are supported without migration.

## Mutation and crash order

1. Acquire the native directory lock. Reject an active journal. Check the exact plan, DB/WAL/SHM bytes, logical state, and filesystem guards.
2. Run preremove hooks if requested. Recheck database bytes and all guards. A hook failure causes no retirement.
3. Open the existing registry without schema creation. Start `BEGIN IMMEDIATE`. Recheck registry and filesystem state.
4. Publish the full prepared journal atomically with exclusive creation. Sync the journal and its parent directory before any rename.
5. Rename each exact directory without replacement. Sync its parent and verify the moved identity, marker, and files.
6. Change only the reviewed active rows. Preserve all old trash rows and auxiliary data. Verify the complete expected registry state. Commit with SQLite full synchronous mode.
7. Mark the journal committed. Run postremove hooks in the moved directories. Recheck moved files and registry state, then persist hook status. A postremove failure returns `committed=true`, a failure status and message, and a nonzero CLI exit. It does not roll back the retirement.

The active journal blocks candidate `Manager` writers until recovery, rollback, or explicit finalization. A crash can leave any subset of the exact directories moved. Recovery compares the complete old and new registry states and each original/trash location. If the DB is old, recovery restores moved directories. If the DB is new, recovery reports committed and retains evidence. It never completes an uncommitted retirement. An ambiguous location or changed state is rejected. Hooks are not replayed. If hook completion is unconfirmed after a crash, rollback is available and finalize is rejected.

Interrupted API errors include `committed: Some(false)`, `Some(true)`, or `None` if SQLite commit returned an error and status requires recovery, plus the journal path. A crash has no return value; use recovery to determine the committed state.

## Finalize and rollback

`finalize(database, journal_hash)` accepts only a completed committed journal with confirmed or skipped hook completion. It verifies the full committed registry and moved filesystem again under the native and SQLite writer locks. It atomically archives the journal to the adjacent `.guarded-history-SHA256` path and syncs the parent. It deletes no history or trash. It then permits normal candidate callers and a new dry plan. This supports reconcile → finalize → trash → finalize.

`rollback(database, journal_hash)` restores an active journal. `rollback_history(database, history_path, journal_hash)` restores an exact adjacent committed history archive. History rollback first requires the current full state to equal the archived committed state. It publishes a durable rolling-back active journal before any reverse rename. The original committed history stays unchanged.

Rollback verifies the full closure before moving anything. It refuses occupied original destinations, stale row/schema/auxiliary-table state, changed aliases, marker or file changes, and wrong history paths or hashes. It restores original Rift IDs, parent IDs, paths, timestamps, rowids, markers, and files. A reverse rename or DB commit failure retains recoverable intent. Recovery can finish interrupted rollback. Completed restoration archives its journal without overwriting the original history.

History rollback is intentionally strict: a later unrelated registry write makes that history stale. Use the original build to recover or roll back its journals. No history pruning or trash deletion command is provided. Current native rename support is Linux and macOS; APFS validation is available in this change. Linux runtime validation remains required before Linux use.

## Repeatable synthetic verification

Failure modes are listed in `e2e/guarded-failure-modes.json`. The CLI E2E runner accepts only explicit binaries and creates new synthetic temporary sources and registries:

```sh
cargo build --locked -p rift-cli --features fixture-faults --target-dir target/fixture
python3 e2e/guarded-lifecycle.py --binary target/fixture/debug/rift \
  --legacy-binary /tmp/public-release/rift --legacy-binary-sha256 VERIFIED_PUBLIC_RELEASE_HASH \
  --out /tmp/new-fixture-receipt
```

The release binary must be the verified public 0.0.12 artifact. The runner records scenario results, command output, binary provenance/hashes, before/after DB/WAL/SHM hashes, row/schema state, filesystem hashes, and failure-mode mappings. Keep receipts outside the repository. The `fixture-faults` feature provides explicit synthetic crash/failure checkpoints; it is off in the production build. Build the production candidate with `cargo build --locked -p rift-cli --release` from the clean reviewed commit. `guarded provenance` reports the exact source commit, dirty state, base release, and feature state. Never dispatch a fixture binary against a real registry.
