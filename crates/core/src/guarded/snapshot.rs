use super::*;
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Row {
    pub rowid: i64,
    pub id: String,
    pub parent_id: Option<String>,
    pub path: PathBuf,
    pub created_at: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct State {
    pub rows: Vec<Row>,
    pub trash: Vec<(String, String, i64)>,
    pub schema: Vec<(String, String, Option<String>)>,
    pub auxiliary_tables: Vec<(String, String)>,
    pub user_version: i64,
    pub application_id: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub device: u64,
    pub inode: u64,
    pub mode: u32,
    pub birth: Option<(i64, i64)>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PathIdentity {
    pub path: PathBuf,
    pub identity: Identity,
    pub link: Option<PathBuf>,
    pub link_change_time: Option<(i64, i64)>,
}
fn path_identity(path: &Path) -> Result<PathIdentity> {
    let metadata = fs::symlink_metadata(path)?;
    #[cfg(unix)]
    let link_change_time = if metadata.file_type().is_symlink() {
        Some((metadata.ctime(), metadata.ctime_nsec()))
    } else {
        None
    };
    #[cfg(not(unix))]
    let link_change_time = None;
    Ok(PathIdentity {
        link_change_time,
        path: path.to_path_buf(),
        identity: identity(path)?,
        link: if metadata.file_type().is_symlink() {
            Some(fs::read_link(path)?)
        } else {
            None
        },
    })
}
fn path_chain(path: &Path) -> Result<Vec<PathIdentity>> {
    let mut chain = path.ancestors().collect::<Vec<_>>();
    chain.reverse();
    chain.into_iter().map(path_identity).collect()
}
pub(super) fn verify_ancestors(g: &Guard) -> Result<()> {
    for entry in g.path_chain.iter().filter(|entry| entry.path != g.stored) {
        if path_identity(&entry.path)? != *entry {
            return Err(fail("stored path ancestor or alias identity changed"));
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Guard {
    pub id: String,
    pub stored: PathBuf,
    pub canonical: PathBuf,
    pub alias: Identity,
    pub path_chain: Vec<PathIdentity>,
    pub directory: Identity,
    pub marker_hash: String,
    pub tree_hash: String,
}

pub(super) fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(unix)]
pub(super) fn identity(path: &Path) -> Result<Identity> {
    let meta = fs::symlink_metadata(path)?;
    #[cfg(target_os = "macos")]
    let birth = {
        use std::os::macos::fs::MetadataExt;
        Some((meta.st_birthtime(), meta.st_birthtime_nsec()))
    };
    #[cfg(not(target_os = "macos"))]
    let birth = None;
    Ok(Identity {
        birth,
        device: meta.dev(),
        inode: meta.ino(),
        mode: meta.mode(),
    })
}

// Every entry is length-framed, so different names/contents cannot have the same
// encoding. Symlinks are hashed as links; traversal never follows them.
#[cfg(unix)]
pub(super) fn tree_hash(path: &Path) -> Result<String> {
    let mut digest = Sha256::new();
    let mut entries = walkdir::WalkDir::new(path)
        .follow_links(false)
        .into_iter()
        .collect::<std::result::Result<Vec<_>, _>>()?;
    entries.sort_by(|a, b| a.path().cmp(b.path()));
    for entry in entries {
        let relative = entry
            .path()
            .strip_prefix(path)
            .map_err(|e| fail(e.to_string()))?;
        let name = relative.as_os_str().as_encoded_bytes();
        digest.update((name.len() as u64).to_le_bytes());
        digest.update(name);
        let meta = fs::symlink_metadata(entry.path())?;
        digest.update(meta.mode().to_le_bytes());
        // Preserve filesystem identity as well as bytes across rename/rollback.
        digest.update(meta.dev().to_le_bytes());
        digest.update(meta.ino().to_le_bytes());
        let data = if meta.is_file() {
            fs::read(entry.path())?
        } else if meta.file_type().is_symlink() {
            fs::read_link(entry.path())?
                .as_os_str()
                .as_encoded_bytes()
                .to_vec()
        } else if meta.is_dir() {
            Vec::new()
        } else {
            return Err(fail("unsupported entry in guarded directory"));
        };
        digest.update((data.len() as u64).to_le_bytes());
        digest.update(data);
    }
    Ok(format!("{:x}", digest.finalize()))
}

#[cfg(not(unix))]
pub(super) fn identity(_: &Path) -> Result<Identity> {
    Err(fail(
        "guarded lifecycle requires Unix filesystem identity and locking",
    ))
}
#[cfg(not(unix))]
pub(super) fn tree_hash(_: &Path) -> Result<String> {
    Err(fail(
        "guarded lifecycle requires Unix filesystem identity and locking",
    ))
}

pub(super) fn guard(row: &Row) -> Result<Guard> {
    let canonical = fs::canonicalize(&row.path)?;
    let marker_path = canonical.join(".rift");
    if !fs::symlink_metadata(&marker_path)?.file_type().is_file() {
        return Err(fail("marker must be a regular file"));
    }
    let marker = fs::read(&marker_path)?;
    if std::str::from_utf8(&marker).ok().map(str::trim) != Some(row.id.as_str()) {
        return Err(fail("marker ID does not match reviewed row"));
    }
    if !canonical.is_dir() {
        return Err(fail("guarded path is not a directory"));
    }
    Ok(Guard {
        id: row.id.clone(),
        stored: row.path.clone(),
        alias: identity(&row.path)?,
        path_chain: path_chain(&row.path)?,
        directory: identity(&canonical)?,
        marker_hash: hash(&marker),
        tree_hash: tree_hash(&canonical)?,
        canonical,
    })
}

pub(super) fn moved_guard(g: &Guard, path: &Path) -> Result<()> {
    verify_ancestors(g)?;
    if identity(path)? != g.directory
        || tree_hash(path)? != g.tree_hash
        || hash(&fs::read(path.join(".rift"))?) != g.marker_hash
    {
        return Err(fail("moved directory identity, marker or files changed"));
    }
    Ok(())
}

pub(super) fn state(db: &Connection) -> Result<State> {
    let mut info = db.prepare("PRAGMA table_info(rift)")?;
    let fields = info
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if fields != ["id", "parent_id", "path", "created_at"] {
        return Err(fail(
            "unsupported active registry fields; refusing lossy history",
        ));
    }

    let mut stmt = db.prepare("SELECT rowid,id,parent_id,path,created_at FROM rift ORDER BY id")?;
    let rows = stmt
        .query_map([], |r| {
            Ok(Row {
                rowid: r.get(0)?,
                id: r.get(1)?,
                parent_id: r.get(2)?,
                path: PathBuf::from(r.get::<_, String>(3)?),
                created_at: r.get(4)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut stmt = db.prepare("SELECT id,path,removed_at FROM trash ORDER BY id")?;
    let trash = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut stmt = db.prepare("SELECT type,name,sql FROM sqlite_master ORDER BY type,name")?;
    let schema: Vec<(String, String, Option<String>)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut auxiliary_tables = Vec::new();
    for (kind, name, _) in &schema {
        if kind != "table" || name == "rift" {
            continue;
        }
        let query = format!("SELECT * FROM \"{}\"", name.replace('"', "\"\""));
        let mut statement = db.prepare(&query)?;
        let count = statement.column_count();
        let values = statement
            .query_map([], |row| {
                let mut encoded = Vec::new();
                for index in 0..count {
                    use rusqlite::types::ValueRef;
                    let (tag, bytes) = match row.get_ref(index)? {
                        ValueRef::Null => (0, Vec::new()),
                        ValueRef::Integer(value) => (1, value.to_le_bytes().to_vec()),
                        ValueRef::Real(value) => (2, value.to_bits().to_le_bytes().to_vec()),
                        ValueRef::Text(value) => (3, value.to_vec()),
                        ValueRef::Blob(value) => (4, value.to_vec()),
                    };
                    encoded.push(tag);
                    encoded.extend((bytes.len() as u64).to_le_bytes());
                    encoded.extend(bytes);
                }
                Ok(encoded)
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut values = values;
        values.sort();
        let mut digest = Sha256::new();
        digest.update((values.len() as u64).to_le_bytes());
        for row in values {
            digest.update((row.len() as u64).to_le_bytes());
            digest.update(row);
        }
        auxiliary_tables.push((name.clone(), format!("{:x}", digest.finalize())));
    }
    let user_version = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let application_id = db.query_row("PRAGMA application_id", [], |r| r.get(0))?;
    Ok(State {
        rows,
        trash,
        schema,
        auxiliary_tables,
        user_version,
        application_id,
    })
}

pub(super) fn database_bytes(path: &Path) -> Result<Vec<Option<Vec<u8>>>> {
    [
        path.to_path_buf(),
        suffix(path, "-wal"),
        suffix(path, "-shm"),
    ]
    .iter()
    .map(|p| match fs::read(p) {
        Ok(data) => Ok(Some(data)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && p != path => Ok(None),
        Err(e) => Err(e.into()),
    })
    .collect()
}

pub(super) fn hashes(bytes: &[Option<Vec<u8>>]) -> Vec<Option<String>> {
    bytes.iter().map(|b| b.as_ref().map(|b| hash(b))).collect()
}

// Read the main DB and live WAL twice. Never open SQLite on the source, never
// create source SHM, and never use immutable=1 (which can ignore live WAL).
pub(super) fn copied_state(path: &Path) -> Result<(State, Vec<Option<String>>)> {
    let first = database_bytes(path)?;
    checkpoint("snapshot_pause")?;
    let temp = tempfile::TempDir::new()?;
    let copy = temp.path().join("snapshot.sqlite");
    for (index, extension) in ["", "-wal", "-shm"].iter().enumerate() {
        if let Some(bytes) = &first[index] {
            fs::write(suffix(&copy, extension), bytes)?;
        }
    }
    let db = Connection::open(&copy)?;
    let state = state(&db)?;
    if database_bytes(path)? != first {
        return Err(fail("database changed during dry snapshot"));
    }
    Ok((state, hashes(&first)))
}
