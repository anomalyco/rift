//! Exact reviewed lifecycle operations. No Manager is opened for dry preflight.
//! Supported on Unix. Plans and journals are review artifacts, not authorization
//! tokens; callers must review the whole plan and restrict access to these files.
mod journal;
mod plan;
mod snapshot;
use crate::{Error, Result, config, hook, id::RiftId};
pub use plan::{Destination, Kind, Outcome, PathChange, Plan, Provenance, Request, provenance};
use plan::{expected_after, selected, validate};
use rusqlite::{Connection, OpenFlags, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
pub use snapshot::{Guard, Identity, PathIdentity, Row, State};
#[cfg(unix)]
use std::os::fd::AsRawFd;
use std::{
    fs,
    path::{Path, PathBuf},
};

fn fail(message: impl Into<String>) -> Error {
    Error::Guarded(message.into())
}
fn suffix(path: &Path, text: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(text);
    PathBuf::from(name)
}

/// Advisory native writer lock on the registry parent directory. This avoids
/// macOS SQLite's database-inode flock locking style and creates no lock file.
/// Registries in one directory are serialized; SQLite BEGIN IMMEDIATE excludes
/// non-cooperating SQL writers during moves.
#[cfg(unix)]
pub(crate) struct WriterLock {
    _file: fs::File,
}
#[cfg(unix)]
impl WriterLock {
    pub(crate) fn acquire(database: &Path, create: bool) -> Result<Self> {
        let parent = database
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let file = fs::File::open(parent)?;
        let mut acquired = false;
        // A concurrent fork can briefly inherit a CLOEXEC descriptor until exec.
        // Bound the wait; never assume a caller will eventually release ownership.
        for attempt in 0..25 {
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                acquired = true;
                break;
            }
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::WouldBlock {
                return Err(error.into());
            }
            if attempt < 24 {
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        }
        if !acquired {
            return Err(fail(
                "another native caller holds the registry directory lock",
            ));
        }
        if fs::metadata(parent)?.ino() != file.metadata()?.ino()
            || fs::metadata(parent)?.dev() != file.metadata()?.dev()
        {
            return Err(fail("registry directory changed during lock acquisition"));
        }
        match fs::symlink_metadata(database) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(fail("database cannot be a symlink"));
            }
            Err(e) if !create || e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
            _ => {}
        }
        Ok(Self { _file: file })
    }
}
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
#[cfg(not(unix))]
pub(crate) struct WriterLock;
#[cfg(not(unix))]
impl WriterLock {
    pub(crate) fn acquire(_: &Path, _: bool) -> Result<Self> {
        Ok(Self)
    }
}

pub(crate) fn reject_journal(database: &Path) -> Result<()> {
    if fs::symlink_metadata(suffix(database, ".guarded-journal")).is_ok() {
        return Err(fail(
            "guarded journal exists; use guarded recovery or rollback first",
        ));
    }
    Ok(())
}

/// Produces a dry review plan from a stable copied DB/WAL/SHM, with no source
/// SQLite connection, schema change, hook, marker write or lock acquisition.
pub fn preflight(database: impl AsRef<Path>, request: Request) -> Result<Plan> {
    let database = fs::canonicalize(database)?;
    reject_journal(&database)?;
    let database_identity = snapshot::identity(&database)?;
    let (before, database_hashes) = snapshot::copied_state(&database)?;
    let ids = selected(&request);
    let guards = ids
        .iter()
        .map(|id| {
            snapshot::guard(
                before
                    .rows
                    .iter()
                    .find(|r| &r.id == id)
                    .ok_or_else(|| fail("unknown selected ID"))?,
            )
        })
        .collect::<Result<Vec<_>>>()?;
    let plan = Plan {
        version: 1,
        provenance: provenance(),
        database,
        database_identity,
        database_hashes,
        before,
        guards,
        request,
    };
    validate(&plan, true)?;
    if snapshot::hashes(&snapshot::database_bytes(&plan.database)?) != plan.database_hashes {
        return Err(fail("database changed during filesystem preflight"));
    }
    revalidate_files(&plan)?;
    Ok(plan)
}
fn revalidate_files(plan: &Plan) -> Result<()> {
    for g in &plan.guards {
        let row = plan.before.rows.iter().find(|r| r.id == g.id).unwrap();
        if snapshot::guard(row)? != *g {
            return Err(fail(
                "filesystem, marker, canonical path or alias guard changed",
            ));
        }
    }
    Ok(())
}
fn open_existing(path: &Path) -> Result<Connection> {
    let db = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    db.busy_timeout(std::time::Duration::ZERO)?;
    db.execute_batch("PRAGMA foreign_keys=ON; PRAGMA synchronous=FULL;")?;
    Ok(db)
}
fn set_rows(db: &Connection, from: &State, to: &State) -> Result<()> {
    // Disable cascading effects by deleting descendants first. Parent IDs and
    // timestamps are restored from complete history, never regenerated.
    let removed: Vec<_> = from
        .rows
        .iter()
        .filter(|r| !to.rows.iter().any(|t| t.id == r.id))
        .collect();
    let mut pending = removed.clone();
    while !pending.is_empty() {
        let leaves: Vec<_> = pending
            .iter()
            .filter(|r| !pending.iter().any(|c| c.parent_id.as_ref() == Some(&r.id)))
            .cloned()
            .collect();
        if leaves.is_empty() {
            return Err(fail("cyclic registry ancestry"));
        }
        for row in leaves {
            db.execute("DELETE FROM rift WHERE id=?1", [&row.id])?;
            pending.retain(|r| r.id != row.id);
        }
    }
    let mut pending: Vec<_> = to
        .rows
        .iter()
        .filter(|r| !from.rows.iter().any(|t| t.id == r.id))
        .collect();
    while !pending.is_empty() {
        let ready: Vec<_> = pending
            .iter()
            .filter(|r| !pending.iter().any(|p| Some(&p.id) == r.parent_id.as_ref()))
            .cloned()
            .collect();
        if ready.is_empty() {
            return Err(fail("cyclic rollback ancestry"));
        }
        for row in ready {
            db.execute(
                "INSERT INTO rift(rowid,id,parent_id,path,created_at) VALUES(?1,?2,?3,?4,?5)",
                params![
                    row.rowid,
                    row.id,
                    row.parent_id,
                    row.path.to_str().ok_or_else(|| fail("non UTF-8 path"))?,
                    row.created_at
                ],
            )?;
            pending.retain(|r| r.id != row.id);
        }
    }
    for row in &to.rows {
        if let Some(old) = from.rows.iter().find(|r| r.id == row.id)
            && old.path != row.path
        {
            db.execute(
                "UPDATE rift SET path=?1 WHERE id=?2",
                params![
                    row.path.to_str().ok_or_else(|| fail("non UTF-8 path"))?,
                    row.id
                ],
            )?;
        }
    }
    Ok(())
}

fn hooks(plan: &Plan, name: &str) -> Result<()> {
    if let Request::Trash { hooks: true, .. } = &plan.request {
        for g in &plan.guards {
            let row = plan.before.rows.iter().find(|r| r.id == g.id).unwrap();
            let current = if name == "preremove" {
                g.canonical.clone()
            } else {
                match &plan.request {
                    Request::Trash { destinations, .. } => destinations
                        .iter()
                        .find(|d| d.id == g.id)
                        .unwrap()
                        .trash
                        .clone(),
                    _ => unreachable!(),
                }
            };
            let config = config::Config::load(&current)?;
            let steps = if name == "preremove" {
                config.preremove()
            } else {
                config.postremove()
            };
            hook::run(
                name,
                steps,
                &current,
                &g.canonical,
                &current,
                &RiftId::from_stored(g.id.clone()),
                &RiftId::from_stored(row.parent_id.clone().unwrap_or_else(|| g.id.clone())),
            )?;
        }
    }
    Ok(())
}

/// Apply exactly this plan. A successful retirement keeps its journal until
/// rollback. Native Manager writers remain blocked while the journal exists.
pub fn apply(plan: &Plan) -> Result<Outcome> {
    let _lock = WriterLock::acquire(&plan.database, false)?;
    reject_journal(&plan.database)?;
    if snapshot::identity(&plan.database)? != plan.database_identity
        || snapshot::hashes(&snapshot::database_bytes(&plan.database)?) != plan.database_hashes
    {
        return Err(fail("stale database identity or byte snapshot"));
    }
    validate(plan, true)?;
    revalidate_files(plan)?;
    if snapshot::copied_state(&plan.database)?.0 != plan.before {
        return Err(fail(
            "reviewed registry state differs from immutable snapshot",
        ));
    }
    hooks(plan, "preremove")?;
    // Hooks can invoke outside tools. Recheck bytes, rows and all filesystem guards.
    if snapshot::hashes(&snapshot::database_bytes(&plan.database)?) != plan.database_hashes {
        return Err(fail("preremove changed database"));
    }
    validate(plan, true)?;
    revalidate_files(plan)?;
    let mut db = open_existing(&plan.database)?;
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if snapshot::state(&tx)? != plan.before {
        return Err(fail("registry changed before writer transaction"));
    }
    revalidate_files(plan)?;
    let mut journal = journal::Journal::new(plan.clone());
    let mut committed = Some(false);
    let transition: Result<()> = (|| {
        checkpoint("journal_publish_failure")?;
        journal::create(&journal)?;
        checkpoint("before_rename")?;
        journal::move_forward(&mut journal)?;
        checkpoint("after_rename")?;
        set_rows(&tx, &plan.before, &expected_after(plan))?;
        checkpoint("database_failure")?;
        if snapshot::state(&tx)? != expected_after(plan) {
            return Err(fail(
                "registry transition invariant failed; recover journal",
            ));
        }
        checkpoint("before_commit")?;
        committed = None;
        tx.commit()?;
        committed = Some(true);
        checkpoint("after_commit")?;
        journal.phase = journal::Phase::Committed;
        journal::save(&journal)?;
        Ok(())
    })();
    if let Err(error) = transition {
        return Err(Error::GuardedInterrupted {
            committed,
            journal: suffix(&plan.database, ".guarded-journal"),
            message: error.to_string(),
        });
    }
    let mut hook_error = hooks(plan, "postremove").err().map(|e| e.to_string());
    if let Err(error) = journal::validate_locations(&journal).and_then(|_| {
        if snapshot::state(&db)? != expected_after(plan) {
            return Err(fail("postremove changed registry state"));
        }
        Ok(())
    }) {
        hook_error = Some(format!("{}; {error}", hook_error.unwrap_or_default()));
    }
    journal.postremove = if let Some(message) = &hook_error {
        journal::HookStatus::Failed(message.clone())
    } else if matches!(plan.request, Request::Trash { hooks: true, .. }) {
        journal::HookStatus::Succeeded
    } else {
        journal::HookStatus::Skipped
    };
    journal::save(&journal).map_err(|error| Error::GuardedInterrupted {
        committed: Some(true),
        journal: suffix(&plan.database, ".guarded-journal"),
        message: error.to_string(),
    })?;
    Ok(Outcome {
        committed: true,
        status: if hook_error.is_some() {
            "committed_hook_failed"
        } else {
            "committed"
        }
        .into(),
        journal: Some(suffix(&plan.database, ".guarded-journal")),
        hook_error,
    })
}

/// Resolve an interrupted operation by comparing complete old/new registry
/// states, then return moved directories to their original locations if the
/// registry did not commit. Committed operations retain the journal for rollback.
pub fn recover(database: impl AsRef<Path>) -> Result<Outcome> {
    journal::recover(database.as_ref(), false, None)
}
/// Roll back only the exact journal hash and current full registry/filesystem
/// snapshot. Occupied destinations and changed files/markers fail closed.
pub fn rollback(database: impl AsRef<Path>, expected_journal_hash: &str) -> Result<Outcome> {
    journal::recover(database.as_ref(), true, Some(expected_journal_hash))
}

/// Verify a completed operation and durably archive its full recovery history.
/// Pending journals cannot be finalized. Archived history remains rollbackable.
pub fn finalize(database: impl AsRef<Path>, expected_journal_hash: &str) -> Result<Outcome> {
    journal::finalize(database.as_ref(), expected_journal_hash)
}
/// Start exact rollback from a reviewed adjacent committed history archive.
pub fn rollback_history(
    database: impl AsRef<Path>,
    history: impl AsRef<Path>,
    expected_journal_hash: &str,
) -> Result<Outcome> {
    journal::rollback_history(database.as_ref(), history.as_ref(), expected_journal_hash)
}

#[cfg(feature = "fixture-faults")]
fn checkpoint(name: &str) -> Result<()> {
    if std::env::var("RIFT_FIXTURE_FAULT").ok().as_deref() == Some(name) {
        if name == "snapshot_pause" {
            std::thread::sleep(std::time::Duration::from_millis(400));
            return Ok(());
        }
        if name == "database_failure"
            || name == "rename_failure"
            || name == "rollback_commit_failure"
            || name == "journal_publish_failure"
        {
            return Err(fail(format!("fixture fault: {name}")));
        }
        std::process::exit(86);
    }
    Ok(())
}
#[cfg(not(feature = "fixture-faults"))]
fn checkpoint(_: &str) -> Result<()> {
    Ok(())
}
