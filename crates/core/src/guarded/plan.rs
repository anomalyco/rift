use super::*;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Provenance {
    pub source_commit: String,
    pub source_dirty: bool,
    pub base_release: String,
    pub base_release_commit: String,
    pub version: String,
    pub fixture_faults: bool,
}
pub fn provenance() -> Provenance {
    Provenance {
        source_commit: env!("RIFT_SOURCE_COMMIT").into(),
        source_dirty: env!("RIFT_SOURCE_DIRTY") == "true",
        base_release: "0.0.12".into(),
        base_release_commit: "6b15e6a4dca5e95362324d5bd1c20134034d2aea".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        fixture_faults: cfg!(feature = "fixture-faults"),
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PathChange {
    pub id: String,
    pub canonical: PathBuf,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Destination {
    pub id: String,
    pub trash: PathBuf,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Root,
    Leaf,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Reconcile {
        paths: Vec<PathChange>,
    },
    Trash {
        kind: Kind,
        id: String,
        closure: Vec<String>,
        destinations: Vec<Destination>,
        hooks: bool,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub version: u32,
    pub provenance: Provenance,
    pub database: PathBuf,
    pub database_identity: Identity,
    pub database_hashes: Vec<Option<String>>,
    pub before: State,
    pub guards: Vec<Guard>,
    pub request: Request,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Outcome {
    pub committed: bool,
    pub status: String,
    pub journal: Option<PathBuf>,
    pub hook_error: Option<String>,
}

pub(super) fn selected(request: &Request) -> Vec<String> {
    match request {
        Request::Reconcile { paths } => paths.iter().map(|p| p.id.clone()).collect(),
        Request::Trash { closure, .. } => closure.clone(),
    }
}
fn unique(ids: &[String]) -> bool {
    let set: std::collections::BTreeSet<_> = ids.iter().collect();
    set.len() == ids.len()
}
pub(super) fn validate(plan: &Plan, live: bool) -> Result<()> {
    if plan.provenance != provenance() {
        return Err(fail("plan was reviewed for a different native build"));
    }
    if plan.version != 1 {
        return Err(fail("unsupported plan version"));
    }
    for row in &plan.before.rows {
        let mut chain = std::collections::BTreeSet::new();
        let mut current = Some(row.id.as_str());
        while let Some(id) = current {
            if !chain.insert(id) {
                return Err(fail("cyclic registry ancestry"));
            }
            let r = plan
                .before
                .rows
                .iter()
                .find(|r| r.id == id)
                .ok_or_else(|| fail("missing registry parent"))?;
            current = r.parent_id.as_deref();
        }
    }
    let ids = selected(&plan.request);
    if ids.is_empty() || !unique(&ids) {
        return Err(fail("selection must be explicit, nonempty and unique"));
    }
    if plan.guards.iter().map(|g| g.id.clone()).collect::<Vec<_>>() != ids {
        return Err(fail("guard selection differs"));
    }
    for g in &plan.guards {
        let row = plan
            .before
            .rows
            .iter()
            .find(|r| r.id == g.id)
            .ok_or_else(|| fail("unknown reviewed ID"))?;
        if g.stored != row.path {
            return Err(fail("stored path guard differs"));
        }
    }
    match &plan.request {
        Request::Reconcile { paths } => {
            let targets: std::collections::BTreeSet<_> =
                paths.iter().map(|p| &p.canonical).collect();
            if targets.len() != paths.len() {
                return Err(fail("duplicate canonical reconciliation target"));
            }
            for p in paths {
                let g = plan.guards.iter().find(|g| g.id == p.id).unwrap();
                if p.canonical == g.stored
                    || p.canonical != g.canonical
                    || !p.canonical.is_absolute()
                {
                    return Err(fail("reconcile target is not exact canonical path"));
                }
                if plan
                    .before
                    .rows
                    .iter()
                    .any(|r| r.id != p.id && r.path == p.canonical)
                {
                    return Err(fail("canonical path belongs to another row"));
                }
            }
        }
        Request::Trash {
            kind,
            id,
            closure,
            destinations,
            ..
        } => {
            let root = plan
                .before
                .rows
                .iter()
                .find(|r| &r.id == id)
                .ok_or_else(|| fail("unknown target ID"))?;
            if (*kind == Kind::Root) != root.parent_id.is_none() {
                return Err(fail("ROOT/LEAF kind differs from registry ancestry"));
            }
            let mut descendants = vec![id.clone()];
            loop {
                let next: Vec<_> = plan
                    .before
                    .rows
                    .iter()
                    .filter(|r| {
                        r.parent_id
                            .as_ref()
                            .is_some_and(|p| descendants.contains(p))
                            && !descendants.contains(&r.id)
                    })
                    .map(|r| r.id.clone())
                    .collect();
                if next.is_empty() {
                    break;
                }
                descendants.extend(next);
            }
            descendants.sort();
            let mut exact = closure.clone();
            exact.sort();
            if descendants != exact {
                return Err(fail(
                    "explicit closure differs from exact registered child closure",
                ));
            }
            let dest_ids: Vec<_> = destinations.iter().map(|d| d.id.clone()).collect();
            let mut sorted = dest_ids.clone();
            sorted.sort();
            if !unique(&dest_ids) || sorted != exact {
                return Err(fail("destinations differ from exact closure"));
            }
            for g in &plan.guards {
                // Trash only canonical stored directories; reconciliation is a separate review.
                if g.stored != g.canonical
                    || g.alias != g.directory
                    || (live && fs::symlink_metadata(&g.stored)?.file_type().is_symlink())
                {
                    return Err(fail("reconcile aliases before exact trash"));
                }
                if plan
                    .guards
                    .iter()
                    .any(|other| other.id != g.id && g.canonical.starts_with(&other.canonical))
                {
                    return Err(fail(
                        "physically nested selected directories are unsupported",
                    ));
                }
                for (_, old_path, _) in &plan.before.trash {
                    let old = if live {
                        fs::canonicalize(old_path).unwrap_or_else(|_| PathBuf::from(old_path))
                    } else {
                        PathBuf::from(old_path)
                    };
                    if old.starts_with(&g.canonical) {
                        return Err(fail("old trash history lies inside selected directory"));
                    }
                }
                if plan.database.starts_with(&g.canonical) {
                    return Err(fail("registry must be outside selected directories"));
                }
                for row in &plan.before.rows {
                    if !closure.contains(&row.id) {
                        let actual = if live {
                            fs::canonicalize(&row.path).unwrap_or_else(|_| row.path.clone())
                        } else {
                            row.path.clone()
                        };
                        if actual.starts_with(&g.canonical) {
                            return Err(fail(
                                "unrelated managed directory lies inside selected directory",
                            ));
                        }
                    }
                }
                let dest = &destinations.iter().find(|d| d.id == g.id).unwrap().trash;
                if dest.parent() != g.canonical.parent()
                    || dest.file_name().and_then(|s| s.to_str())
                        != Some(format!(".rift-trash-{}", g.id).as_str())
                {
                    return Err(fail("trash must be explicit adjacent .rift-trash-ID"));
                }
                if live && fs::symlink_metadata(dest).is_ok() {
                    return Err(fail("trash destination is occupied"));
                }
                if plan
                    .before
                    .rows
                    .iter()
                    .any(|r| r.path.starts_with(dest) || dest.starts_with(&r.path))
                {
                    return Err(fail("trash overlaps a managed path"));
                }
            }
        }
    }
    Ok(())
}

pub(super) fn expected_after(plan: &Plan) -> State {
    let mut after = plan.before.clone();
    match &plan.request {
        Request::Reconcile { paths } => {
            for p in paths {
                after.rows.iter_mut().find(|r| r.id == p.id).unwrap().path = p.canonical.clone();
            }
        }
        Request::Trash { closure, .. } => after.rows.retain(|r| !closure.contains(&r.id)),
    }
    after
}
