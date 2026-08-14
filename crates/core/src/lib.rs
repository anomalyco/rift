mod config;
mod filter;
mod git;
mod hook;
mod id;
mod marker;
mod name;
mod registry;
mod strategy;

#[cfg(all(test, target_os = "linux"))]
mod linux_filesystem_tests;
#[cfg(all(test, target_os = "linux"))]
mod test_support;

use id::RiftId;
use name::RiftName;
use registry::{MovedRecord, PathRecord, Record, Registry, SubtreeScope};
use std::fs;
use std::path::{Path, PathBuf};
use strategy::{Strategy, StrategyInit};
use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Database(#[from] rusqlite::Error),
    #[error("{0}")]
    Walk(#[from] walkdir::Error),
    #[error("invalid path: {0}")]
    Path(String),
    #[error("copy-on-write cloning unavailable: {0}")]
    CowUnavailable(String),
    #[error("workspace requires initialization: {0}")]
    InitializationRequired(PathBuf),
    #[error("workspace is not initialized: {0}")]
    WorkspaceNotInitialized(PathBuf),
    #[error("rift marker is missing: {0}")]
    MissingMarker(PathBuf),
    #[error("unsupported filesystem entry: {0}")]
    UnsupportedEntry(PathBuf),
    #[error("unsafe Git source: {0}")]
    UnsafeGit(String),
    #[error("directory is not managed by rift: {0}")]
    NotManaged(PathBuf),
    #[error("rift marker does not match the registry at: {0}")]
    MarkerMismatch(PathBuf),
    #[error("rift marker belongs to an unknown registry entry at: {0}")]
    UnknownMarker(PathBuf),
    #[error("rift directory already exists: {0}")]
    AlreadyExists(PathBuf),
    #[error("cannot remove subtree while a recorded rift path is missing: {0}")]
    MissingRift(PathBuf),
    #[error("cannot copy a workspace into itself: {0}")]
    InsideSource(PathBuf),
    #[error("invalid rift config at {path}: {message}")]
    InvalidConfig { path: PathBuf, message: String },
    #[error("{hook} hook failed at {path}: `{command}` {message}")]
    HookFailed {
        hook: String,
        path: PathBuf,
        command: String,
        message: String,
    },
}

pub struct Create {
    pub from: PathBuf,
    pub name: Option<String>,
    pub into: Option<PathBuf>,
}

impl Create {
    pub fn new(from: impl Into<PathBuf>) -> Self {
        Self {
            from: from.into(),
            name: None,
            into: None,
        }
    }

    pub fn named(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    pub fn with_name(mut self, name: Option<String>) -> Self {
        self.name = name;
        self
    }

    pub fn with_storage(mut self, into: Option<PathBuf>) -> Self {
        self.into = into;
        self
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CreateOptions {
    pub copy_mode: CopyMode,
    pub hook_mode: HookMode,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RemoveOptions {
    pub hook_mode: HookMode,
}

impl RemoveOptions {
    pub fn hook_mode(mut self, hook_mode: HookMode) -> Self {
        self.hook_mode = hook_mode;
        self
    }
}

impl CreateOptions {
    pub fn copy_mode(mut self, copy_mode: CopyMode) -> Self {
        self.copy_mode = copy_mode;
        self
    }

    pub fn hook_mode(mut self, hook_mode: HookMode) -> Self {
        self.hook_mode = hook_mode;
        self
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CopyMode {
    Filtered,
    All,
}

impl Default for CopyMode {
    fn default() -> Self {
        Self::Filtered
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HookMode {
    Run,
    Skip,
}

impl Default for HookMode {
    fn default() -> Self {
        Self::Run
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InitProgress {
    CreatingSubvolume,
    ImportingWorkspace,
    ImportedEntries { entries: u64 },
    ActivatingWorkspace,
    RemovingOriginal,
    RestoringMarker,
    RegisteringWorkspace,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InitOutcome {
    Registered,
    AlreadyInitialized,
    Converted,
}

impl InitOutcome {
    pub fn is_converted(self) -> bool {
        matches!(self, Self::Converted)
    }
}

pub struct Manager {
    registry: Registry,
    strategy: Box<dyn Strategy>,
}

impl Manager {
    pub fn open_default() -> Result<Self> {
        let path = default_database_path()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        Self::open(path)
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::with_strategy(path, strategy::default_strategy())
    }

    fn with_strategy(path: impl AsRef<Path>, strategy: Box<dyn Strategy>) -> Result<Self> {
        let registry = Registry::open(path)?;
        Ok(Self { registry, strategy })
    }

    pub fn create(&mut self, input: Create) -> Result<PathBuf> {
        self.create_with_options(input, CreateOptions::default())
    }

    pub fn create_with_options(
        &mut self,
        input: Create,
        options: CreateOptions,
    ) -> Result<PathBuf> {
        let requested = existing_directory(&input.from)?;
        let source = self.workspace_from(&requested)?;
        let from = source.path.clone();
        let git = git::check_source(&from)?;
        let root = self.root(&source)?;
        let id = RiftId::new();
        let destination_parent = match input.into {
            Some(path) => absolute_path(&path)?,
            None => default_storage(&root.path)?,
        };
        let name = RiftName::from_optional(input.name)?;
        if destination_parent.join(name.as_str()).starts_with(&from) {
            return Err(Error::InsideSource(destination_parent.join(name.as_str())));
        }
        fs::create_dir_all(&destination_parent)?;
        let destination_parent = fs::canonicalize(destination_parent)?;
        let destination = destination_parent.join(name.as_str());
        if destination.starts_with(&from) {
            return Err(Error::InsideSource(destination));
        }
        if destination.exists() {
            return Err(Error::AlreadyExists(destination));
        }
        let config = match options.hook_mode {
            HookMode::Run => config::Config::load(&from)?,
            HookMode::Skip => config::Config::default(),
        };

        hook::run(
            "precreate",
            config.precreate(),
            &from,
            &from,
            &destination,
            &id,
            &source.id,
        )?;

        if let Err(error) = self
            .strategy
            .copy_directory(&from, &destination, options.copy_mode)
        {
            if destination.exists() {
                let _ = self.strategy.remove_directory(&destination);
            }
            return Err(error);
        }

        let result: Result<()> = (|| {
            marker::write(&destination, &id)?;
            if git.is_repository() {
                git::hide_marker(&destination)?;
                git::detach_destination(&destination)?;
            }
            if git.is_repository() {
                git::hide_marker(&from)?;
            }
            self.registry.insert_child(&id, &source.id, &destination)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = self.strategy.remove_directory(&destination);
        }
        result?;
        hook::run(
            "postcreate",
            config.postcreate(),
            &destination,
            &from,
            &destination,
            &id,
            &source.id,
        )?;
        Ok(destination)
    }

    pub fn init(&mut self, at: impl AsRef<Path>) -> Result<InitOutcome> {
        self.init_with_progress(at, |_| {})
    }

    pub fn init_with_progress(
        &mut self,
        at: impl AsRef<Path>,
        mut progress: impl FnMut(InitProgress),
    ) -> Result<InitOutcome> {
        let at = existing_directory(at.as_ref())?;
        let git = git::check_source(&at)?;
        if let Some(record) = self.registry.record_at(&at)? {
            if marker::read(&at)?.is_none() {
                progress(InitProgress::RestoringMarker);
                marker::write(&at, &record.id)?;
            } else {
                marker::verify(&record.path, &record.id)?;
            }
            let converted = self.strategy.initialize_directory(&at, &mut progress)?;
            if git.is_repository() {
                git::hide_marker(&at)?;
            }
            return Ok(match converted {
                StrategyInit::AlreadyNative => InitOutcome::AlreadyInitialized,
                StrategyInit::Converted => InitOutcome::Converted,
            });
        }
        if marker::read(&at)?.is_some() {
            return Err(Error::MarkerMismatch(at));
        }

        let converted = self.strategy.initialize_directory(&at, &mut progress)?;
        progress(InitProgress::RegisteringWorkspace);
        let id = RiftId::new();
        let result = (|| {
            marker::write(&at, &id)?;
            if git.is_repository() {
                git::hide_marker(&at)?;
            }
            self.registry.insert_root(&id, &at)?;
            Ok(match converted {
                StrategyInit::AlreadyNative => InitOutcome::Registered,
                StrategyInit::Converted => InitOutcome::Converted,
            })
        })();
        if result.is_err() {
            let _ = fs::remove_file(marker::path(&at));
        }
        result
    }

    pub fn remove(&mut self, at: impl AsRef<Path>) -> Result<()> {
        self.remove_with_options(at, RemoveOptions::default())
    }

    pub fn remove_with_options(
        &mut self,
        at: impl AsRef<Path>,
        options: RemoveOptions,
    ) -> Result<()> {
        let record = self.workspace_at(at)?;
        marker::verify(&record.path, &record.id)?;
        let config = self.remove_config(&record.path, options)?;
        let parent_id = record.parent_id.as_ref().unwrap_or(&record.id);
        hook::run(
            "preremove",
            config.preremove(),
            &record.path,
            &record.path,
            &record.path,
            &record.id,
            parent_id,
        )?;
        if record.parent_id.is_none() {
            self.unregister_root(&record)?;
            return hook::run(
                "postremove",
                config.postremove(),
                &record.path,
                &record.path,
                &record.path,
                &record.id,
                parent_id,
            );
        }
        let rows = self
            .registry
            .subtree(&record.id, SubtreeScope::IncludingRoot)?;
        self.trash_rows(&rows)?;
        let trashed = trash_path(&record.id, &record.path)?;
        hook::run(
            "postremove",
            config.postremove(),
            &trashed,
            &record.path,
            &trashed,
            &record.id,
            parent_id,
        )
    }

    pub fn remove_all(&mut self, at: impl AsRef<Path>) -> Result<Vec<PathBuf>> {
        self.remove_all_with_options(at, RemoveOptions::default())
    }

    pub fn remove_all_with_options(
        &mut self,
        at: impl AsRef<Path>,
        options: RemoveOptions,
    ) -> Result<Vec<PathBuf>> {
        let record = self.workspace_at(at)?;
        marker::verify(&record.path, &record.id)?;
        let config = self.remove_config(&record.path, options)?;
        let parent_id = record.parent_id.as_ref().unwrap_or(&record.id);
        hook::run(
            "preremove",
            config.preremove(),
            &record.path,
            &record.path,
            &record.path,
            &record.id,
            parent_id,
        )?;
        let rows = self
            .registry
            .subtree(&record.id, SubtreeScope::DescendantsOnly)?;
        self.trash_rows(&rows)?;
        hook::run(
            "postremove",
            config.postremove(),
            &record.path,
            &record.path,
            &record.path,
            &record.id,
            parent_id,
        )?;
        Ok(rows.into_iter().map(|record| record.path).collect())
    }

    fn remove_config(&self, workspace: &Path, options: RemoveOptions) -> Result<config::Config> {
        match options.hook_mode {
            HookMode::Run => config::Config::load(workspace),
            HookMode::Skip => Ok(config::Config::default()),
        }
    }

    fn unregister_root(&mut self, record: &Record) -> Result<()> {
        marker::verify(&record.path, &record.id)?;
        let rows = self
            .registry
            .subtree(&record.id, SubtreeScope::DescendantsOnly)?;
        let existing = rows
            .into_iter()
            .filter(|record| record.path.exists())
            .collect::<Vec<_>>();
        self.trash_rows(&existing)?;
        fs::remove_file(marker::path(&record.path))?;
        self.registry.delete_active(&record.id)?;
        Ok(())
    }

    fn trash_rows(&mut self, rows: &[PathRecord]) -> Result<()> {
        rows.iter().try_for_each(|row| -> Result<()> {
            row.path
                .exists()
                .then_some(())
                .ok_or_else(|| Error::MissingRift(row.path.clone()))?;
            marker::verify(&row.path, &row.id)?;
            Ok(())
        })?;
        let targets = rows
            .iter()
            .map(|row| {
                Ok(MovedRecord {
                    id: row.id.clone(),
                    original_path: row.path.clone(),
                    trash_path: trash_path(&row.id, &row.path)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        targets.iter().try_for_each(|target| {
            (!target.trash_path.exists())
                .then_some(())
                .ok_or_else(|| Error::AlreadyExists(target.trash_path.clone()))
        })?;
        let mut moved: Vec<MovedRecord> = Vec::with_capacity(rows.len());
        for target in targets {
            let trash_parent = target.trash_path.parent().ok_or_else(|| {
                Error::Path(format!(
                    "trash path has no parent: {}",
                    target.trash_path.display()
                ))
            })?;
            fs::create_dir_all(trash_parent)?;
            if let Err(error) = fs::rename(&target.original_path, &target.trash_path) {
                for record in moved.iter().rev() {
                    let _ = fs::rename(&record.trash_path, &record.original_path);
                }
                return Err(error.into());
            }
            moved.push(target);
        }
        let result = self.registry.trash_moved(&moved);
        if result.is_err() {
            for record in moved.iter().rev() {
                let _ = fs::rename(&record.trash_path, &record.original_path);
            }
        }
        result
    }

    pub fn list(&self, of: impl AsRef<Path>) -> Result<Vec<PathBuf>> {
        let record = self.workspace_at(of)?;
        self.registry.child_paths(&record.id)
    }

    pub fn ancestors(&self, of: impl AsRef<Path>) -> Result<Vec<PathBuf>> {
        let record = self.workspace_at(of)?;
        let mut paths = Vec::new();
        let mut parent_id = record.parent_id;
        while let Some(id) = parent_id {
            let parent = self
                .registry
                .record_id(&id)?
                .ok_or_else(|| Error::NotManaged(record.path.clone()))?;
            paths.push(parent.path);
            parent_id = parent.parent_id;
        }
        Ok(paths)
    }

    pub fn gc(&mut self) -> Result<Vec<PathBuf>> {
        let removed = self
            .registry
            .trashed_paths()?
            .into_iter()
            .map(|row| -> Result<PathBuf> {
                if row.path.exists() {
                    self.strategy.remove_directory(&row.path)?;
                }
                self.registry.delete_trash(&row.id)?;
                Ok(row.path)
            })
            .collect::<Result<Vec<_>>>()?;

        let missing = self
            .registry
            .active_paths()?
            .into_iter()
            .filter(|row| !row.path.exists())
            .map(|row| {
                self.registry
                    .subtree(&row.id, SubtreeScope::DescendantsOnly)
                    .map(|descendants| {
                        (!descendants
                            .iter()
                            .any(|descendant| descendant.path.exists()))
                        .then_some(row)
                    })
            })
            .filter_map(Result::transpose)
            .collect::<Result<Vec<_>>>()?;
        self.registry.delete_active_records(&missing)?;
        Ok(removed
            .into_iter()
            .chain(missing.into_iter().map(|record| record.path))
            .collect())
    }

    pub fn workspace(&self, at: impl AsRef<Path>) -> Result<PathBuf> {
        Ok(self.workspace_at(at)?.path)
    }

    fn workspace_at(&self, path: impl AsRef<Path>) -> Result<Record> {
        let path = existing_directory(path.as_ref())?;
        self.workspace_from(&path)
    }

    fn workspace_from(&self, path: &Path) -> Result<Record> {
        self.workspace_from_optional(path)?
            .ok_or_else(|| Error::WorkspaceNotInitialized(path.to_path_buf()))
    }

    fn workspace_from_optional(&self, path: &Path) -> Result<Option<Record>> {
        for directory in path.ancestors() {
            if let Some(id) = marker::read(directory)? {
                let record = self
                    .registry
                    .record_id(&id)?
                    .ok_or_else(|| Error::UnknownMarker(directory.to_path_buf()))?;
                if record.path != directory {
                    return Err(Error::MarkerMismatch(directory.to_path_buf()));
                }
                return Ok(Some(record));
            }
            if self.registry.record_at(directory)?.is_some() {
                return Err(Error::MissingMarker(directory.to_path_buf()));
            }
        }
        Ok(None)
    }

    fn root(&self, record: &Record) -> Result<Record> {
        let mut current = record.clone();
        while let Some(id) = current.parent_id.clone() {
            current = self
                .registry
                .record_id(&id)?
                .ok_or_else(|| Error::NotManaged(record.path.clone()))?;
        }
        Ok(current)
    }
}

fn default_database_path() -> Result<PathBuf> {
    let base = dirs::data_local_dir()
        .ok_or_else(|| Error::Path("user data directory is unavailable".into()))?;
    Ok(base.join("rift").join("rift.sqlite"))
}

fn existing_directory(path: &Path) -> Result<PathBuf> {
    let path = fs::canonicalize(path)?;
    if !path.is_dir() {
        return Err(Error::Path(format!("not a directory: {}", path.display())));
    }
    Ok(path)
}

fn absolute_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    Ok(std::env::current_dir()?.join(path))
}

fn default_storage(root: &Path) -> Result<PathBuf> {
    let parent = root
        .parent()
        .ok_or_else(|| Error::Path(format!("workspace has no parent: {}", root.display())))?;
    let name = root
        .file_name()
        .ok_or_else(|| Error::Path(format!("workspace has no name: {}", root.display())))?;
    Ok(parent.join(".rifts").join(name))
}

fn trash_path(id: &RiftId, path: &Path) -> Result<PathBuf> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::Path(format!("rift has no parent: {}", path.display())))?;
    let name = path
        .file_name()
        .ok_or_else(|| Error::Path(format!("rift has no name: {}", path.display())))?;
    Ok(parent
        .join(".trash")
        .join(format!("{id}-{}", name.to_string_lossy())))
}

#[cfg(test)]
mod tests;
