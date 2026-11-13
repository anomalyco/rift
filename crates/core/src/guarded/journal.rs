use super::*;
use std::io::Write;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum Phase {
    Prepared,
    Committed,
    RollingBack,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) enum HookStatus {
    Pending,
    Skipped,
    Succeeded,
    Failed(String),
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Journal {
    pub version: u32,
    pub phase: Phase,
    pub plan: Plan,
    pub postremove: HookStatus,
}
impl Journal {
    pub fn new(plan: Plan) -> Self {
        Self {
            version: 1,
            phase: Phase::Prepared,
            postremove: if matches!(plan.request, Request::Trash { hooks: true, .. }) {
                HookStatus::Pending
            } else {
                HookStatus::Skipped
            },
            plan,
        }
    }
}
fn path(j: &Journal) -> PathBuf {
    suffix(&j.plan.database, ".guarded-journal")
}
fn bytes(j: &Journal) -> Result<Vec<u8>> {
    serde_json::to_vec_pretty(j).map_err(|e| fail(e.to_string()))
}
fn sync_parent(path: &Path) -> Result<()> {
    fs::File::open(path.parent().ok_or_else(|| fail("path has no parent"))?)?.sync_all()?;
    Ok(())
}
pub(super) fn create(j: &Journal) -> Result<()> {
    let p = path(j);
    let mut temp = tempfile::NamedTempFile::new_in(p.parent().unwrap())?;
    temp.write_all(&bytes(j)?)?;
    temp.as_file().sync_all()?;
    temp.persist_noclobber(&p).map_err(|e| Error::Io(e.error))?;
    sync_parent(&p)
}
pub(super) fn save(j: &Journal) -> Result<()> {
    let p = path(j);
    let mut temp = tempfile::NamedTempFile::new_in(p.parent().unwrap())?;
    temp.write_all(&bytes(j)?)?;
    temp.as_file().sync_all()?;
    temp.persist(&p).map_err(|e| Error::Io(e.error))?;
    sync_parent(&p)
}

// Atomic no-replace rename: never overwrite a destination even if it appears
// after validation. No fallback to deleting or ordinary overwriting rename.
fn rename_exact(from: &Path, to: &Path) -> Result<()> {
    use std::ffi::CString;
    let src = CString::new(from.as_os_str().as_encoded_bytes()).map_err(|e| fail(e.to_string()))?;
    let dst = CString::new(to.as_os_str().as_encoded_bytes()).map_err(|e| fail(e.to_string()))?;
    #[cfg(target_os = "macos")]
    let rc = unsafe { libc::renamex_np(src.as_ptr(), dst.as_ptr(), libc::RENAME_EXCL) };
    #[cfg(target_os = "linux")]
    let rc = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            src.as_ptr(),
            libc::AT_FDCWD,
            dst.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let rc = -1;
    if rc != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    sync_parent(from)?;
    if from.parent() != to.parent() {
        sync_parent(to)?;
    }
    Ok(())
}
fn moves(plan: &Plan) -> Vec<(&Guard, &Path)> {
    match &plan.request {
        Request::Reconcile { .. } => Vec::new(),
        Request::Trash { destinations, .. } => plan
            .guards
            .iter()
            .map(|g| {
                (
                    g,
                    destinations
                        .iter()
                        .find(|d| d.id == g.id)
                        .unwrap()
                        .trash
                        .as_path(),
                )
            })
            .collect(),
    }
}
pub(super) fn move_forward(j: &mut Journal) -> Result<()> {
    for (g, to) in moves(&j.plan) {
        snapshot::moved_guard(g, &g.canonical)?;
        checkpoint("rename_failure")?;
        rename_exact(&g.canonical, to)?;
        snapshot::moved_guard(g, to)?;
        checkpoint("after_each_rename")?;
    }
    Ok(())
}
fn locations(j: &Journal) -> Result<Vec<bool>> {
    moves(&j.plan)
        .iter()
        .map(|(g, to)| {
            let original = fs::symlink_metadata(&g.canonical).is_ok();
            let trash = fs::symlink_metadata(to).is_ok();
            if original == trash {
                return Err(fail(
                    "ambiguous recovery: original/trash missing or occupied",
                ));
            }
            snapshot::moved_guard(g, if original { &g.canonical } else { to })?;
            Ok(trash)
        })
        .collect()
}
pub(super) fn validate_locations(j: &Journal) -> Result<()> {
    if locations(j)?.iter().any(|m| !*m) {
        return Err(fail("retired directory returned outside rollback"));
    }
    if matches!(j.plan.request, Request::Reconcile { .. }) {
        validate_reconciled(j)?;
    }
    Ok(())
}
fn move_back(j: &Journal) -> Result<()> {
    // Prevalidate the entire closure before any rollback rename.
    let moved = locations(j)?;
    for ((g, to), is_moved) in moves(&j.plan).iter().zip(moved).rev() {
        if is_moved {
            snapshot::moved_guard(g, to)?;
            rename_exact(to, &g.canonical)?;
            snapshot::moved_guard(g, &g.canonical)?;
            checkpoint("during_rollback")?;
        }
    }
    Ok(())
}
fn finish(j: &Journal) -> Result<PathBuf> {
    let p = path(j);
    let data = fs::read(&p)?;
    let archive = suffix(
        &j.plan.database,
        &format!(".guarded-history-{}", snapshot::hash(&data)),
    );
    rename_exact(&p, &archive)?;
    Ok(archive)
}

pub(super) fn recover(
    database: &Path,
    rollback: bool,
    expected_hash: Option<&str>,
) -> Result<Outcome> {
    let database = fs::canonicalize(database)?;
    let _lock = WriterLock::acquire(&database, false)?;
    recover_locked(&database, rollback, expected_hash)
}
fn recover_locked(database: &Path, rollback: bool, expected_hash: Option<&str>) -> Result<Outcome> {
    let p = suffix(database, ".guarded-journal");
    if !fs::symlink_metadata(&p)?.file_type().is_file() {
        return Err(fail("journal must be a regular file"));
    }
    let data = fs::read(&p)?;
    if let Some(hash) = expected_hash
        && hash != snapshot::hash(&data)
    {
        return Err(fail("stale reviewed rollback journal hash"));
    }
    let mut j: Journal = serde_json::from_slice(&data).map_err(|e| fail(e.to_string()))?;
    if j.version != 1
        || j.plan.database != database
        || snapshot::identity(database)? != j.plan.database_identity
    {
        return Err(fail("journal database identity differs"));
    }
    validate(&j.plan, false)?;
    // Request validation at this stage must not require original directories to
    // exist: those may already have moved. Validate trusted saved state below.
    let (dry_state, dry_hashes) = snapshot::copied_state(database)?;
    let after = expected_after(&j.plan);
    if dry_state != j.plan.before && dry_state != after {
        return Err(fail("stale registry recovery/rollback snapshot"));
    }
    let dry_moved = locations(&j)?;
    if dry_state == after
        && !rollback
        && j.phase != Phase::RollingBack
        && dry_moved.iter().any(|m| !*m)
    {
        return Err(fail("committed registry has unretired directories"));
    }
    if matches!(j.plan.request, Request::Reconcile { .. }) {
        validate_reconciled(&j)?;
    }
    if snapshot::hashes(&snapshot::database_bytes(database)?) != dry_hashes {
        return Err(fail("registry changed during recovery preflight"));
    }
    let mut db = open_existing(database)?;
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let current = snapshot::state(&tx)?;
    let after = expected_after(&j.plan);
    if current != dry_state || (current != j.plan.before && current != after) {
        return Err(fail("stale registry recovery/rollback snapshot"));
    }
    let moved = locations(&j)?;
    let committed = current == after;
    let mut status = Some(committed);
    let transition: Result<Outcome> = (|| {
        if committed && !rollback && j.phase != Phase::RollingBack {
            if moved.iter().any(|m| !*m) {
                return Err(fail("committed registry has unretired directories"));
            }
            // Reconciliation also needs current marker, alias and file guards.
            if matches!(j.plan.request, Request::Reconcile { .. }) {
                validate_reconciled(&j)?;
            }
            tx.commit()?;
            j.phase = Phase::Committed;
            save(&j)?;
            return Ok(Outcome {
                committed: true,
                status: match &j.postremove {
                    HookStatus::Pending => "committed_hooks_unconfirmed",
                    HookStatus::Failed(_) => "committed_hook_failed",
                    _ => "committed",
                }
                .into(),
                journal: Some(p.clone()),
                hook_error: match &j.postremove {
                    HookStatus::Pending => {
                        Some("postremove completion is unconfirmed; hooks are not replayed".into())
                    }
                    HookStatus::Failed(message) => Some(message.clone()),
                    _ => None,
                },
            });
        }
        if rollback || j.phase == Phase::RollingBack {
            if matches!(j.plan.request, Request::Reconcile { .. }) {
                validate_reconciled(&j)?;
            }
            j.phase = Phase::RollingBack;
            save(&j)?;
        }
        move_back(&j)?;
        if current != j.plan.before {
            set_rows(&tx, &current, &j.plan.before)?;
        }
        if snapshot::state(&tx)? != j.plan.before {
            return Err(fail("rollback registry invariant failed"));
        }
        checkpoint("rollback_commit_failure")?;
        status = None;
        tx.commit()?;
        status = Some(false);
        checkpoint("after_rollback_commit")?;
        let archive = finish(&j)?;
        Ok(Outcome {
            committed: false,
            status: "restored".into(),
            journal: Some(archive),
            hook_error: None,
        })
    })();
    transition.map_err(|error| Error::GuardedInterrupted {
        committed: status,
        journal: p,
        message: error.to_string(),
    })
}
fn validate_reconciled(j: &Journal) -> Result<()> {
    for g in &j.plan.guards {
        let row = j
            .plan
            .before
            .rows
            .iter()
            .find(|r| r.id == g.id)
            .ok_or_else(|| fail("unknown journal guard"))?;
        if snapshot::guard(row)? != *g {
            return Err(fail("rollback alias, directory, marker or files changed"));
        }
    }
    Ok(())
}

fn reviewed_journal(database: &Path, path: &Path, expected_hash: &str) -> Result<Journal> {
    if !fs::symlink_metadata(path)?.file_type().is_file() {
        return Err(fail("history/journal must be a regular file"));
    }
    let data = fs::read(path)?;
    if snapshot::hash(&data) != expected_hash {
        return Err(fail("stale reviewed journal hash"));
    }
    let j: Journal = serde_json::from_slice(&data).map_err(|e| fail(e.to_string()))?;
    if j.version != 1
        || j.plan.database != database
        || snapshot::identity(database)? != j.plan.database_identity
    {
        return Err(fail("journal database identity differs"));
    }
    validate(&j.plan, false)?;
    if matches!(j.postremove, HookStatus::Pending) {
        return Err(fail(
            "postremove completion is unconfirmed; rollback remains available",
        ));
    }
    if j.phase != Phase::Committed {
        return Err(fail(
            "only a completed committed journal can be finalized or restored from history",
        ));
    }
    let (current, hashes) = snapshot::copied_state(database)?;
    if current != expected_after(&j.plan) {
        return Err(fail("stale committed registry snapshot"));
    }
    validate_locations(&j)?;
    if snapshot::hashes(&snapshot::database_bytes(database)?) != hashes {
        return Err(fail("registry changed during journal review"));
    }
    Ok(j)
}

pub(super) fn finalize(database: &Path, expected_hash: &str) -> Result<Outcome> {
    let database = fs::canonicalize(database)?;
    let _lock = WriterLock::acquire(&database, false)?;
    let p = suffix(&database, ".guarded-journal");
    let j = reviewed_journal(&database, &p, expected_hash)?;
    let mut db = open_existing(&database)?;
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if snapshot::state(&tx)? != expected_after(&j.plan) {
        return Err(fail("registry changed before finalization"));
    }
    validate_locations(&j)?;
    let archive = finish(&j).map_err(|e| Error::GuardedInterrupted {
        committed: Some(true),
        journal: p.clone(),
        message: e.to_string(),
    })?;
    checkpoint("after_finalize_archive")?;
    tx.commit().map_err(|e| Error::GuardedInterrupted {
        committed: Some(true),
        journal: archive.clone(),
        message: e.to_string(),
    })?;
    Ok(Outcome {
        committed: true,
        status: "finalized".into(),
        journal: Some(archive),
        hook_error: None,
    })
}

pub(super) fn rollback_history(
    database: &Path,
    history: &Path,
    expected_hash: &str,
) -> Result<Outcome> {
    let database = fs::canonicalize(database)?;
    let _lock = WriterLock::acquire(&database, false)?;
    reject_journal(&database)?;
    let exact = suffix(&database, &format!(".guarded-history-{expected_hash}"));
    if history != exact {
        return Err(fail(
            "history path is not the exact adjacent reviewed archive",
        ));
    }
    let mut j = reviewed_journal(&database, history, expected_hash)?;
    j.phase = Phase::RollingBack;
    // Publish rollback intent before the first reverse rename; preserve the
    // original committed history unchanged for audit and crash recovery.
    create(&j)?;
    recover_locked(&database, true, None)
}
