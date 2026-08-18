use super::*;
use crate::strategy::{FailureStrategy, Strategy, TestStrategy};
use std::cell::Cell;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use std::rc::Rc;
use tempfile::TempDir;
use ulid::Ulid;

fn manager(temp: &TempDir) -> Manager {
    Manager::with_strategy(temp.path().join("registry.sqlite"), Box::new(TestStrategy)).unwrap()
}

fn source(temp: &TempDir) -> PathBuf {
    let source = temp.path().join("app");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("file.txt"), "hello").unwrap();
    fs::canonicalize(source).unwrap()
}

fn marker_id(path: &Path) -> RiftId {
    marker::read(path).unwrap().unwrap()
}

fn create_input(from: PathBuf, name: &str) -> Create {
    Create::new(from).named(name)
}

fn create_options(copy_mode: CopyMode, hook_mode: HookMode) -> CreateOptions {
    CreateOptions::default()
        .copy_mode(copy_mode)
        .hook_mode(hook_mode)
}

fn child_path(source: &Path, name: &str) -> PathBuf {
    source.parent().unwrap().join(".rifts/app").join(name)
}

#[test]
fn create_tracks_parentage_and_default_storage() {
    let temp = TempDir::new().unwrap();
    let source = source(&temp);
    let mut manager = manager(&temp);
    manager.init(&source).unwrap();
    let first = manager
        .create(Create::new(source.clone()).named("first"))
        .unwrap();
    let second = manager
        .create(Create::new(first.clone()).named("second"))
        .unwrap();

    let parent = source.parent().unwrap();
    assert_eq!(first, parent.join(".rifts/app/first"));
    assert_eq!(second, parent.join(".rifts/app/second"));
    assert_ne!(
        fs::read_to_string(source.join(".rift")).unwrap(),
        fs::read_to_string(first.join(".rift")).unwrap()
    );
    assert_eq!(manager.list(&source).unwrap(), vec![first.clone()]);
    assert_eq!(manager.ancestors(&second).unwrap(), vec![first, source]);
}

#[test]
fn init_registers_a_root_workspace_without_creating_a_child() {
    let temp = TempDir::new().unwrap();
    let source = source(&temp);
    let mut manager = manager(&temp);

    assert_eq!(manager.init(&source).unwrap(), InitOutcome::Registered);
    assert!(source.join(".rift").exists());
    assert!(manager.list(&source).unwrap().is_empty());
    assert_eq!(
        manager.init(&source).unwrap(),
        InitOutcome::AlreadyInitialized
    );
}

#[test]
fn init_reports_structured_registration_progress_when_requested() {
    let temp = TempDir::new().unwrap();
    let source = source(&temp);
    let mut manager = manager(&temp);
    let mut progress = Vec::new();

    manager
        .init_with_progress(&source, |event| progress.push(event))
        .unwrap();

    assert_eq!(progress, vec![InitProgress::RegisteringWorkspace]);
}

#[test]
fn create_supports_custom_storage_and_rejects_invalid_destinations() {
    let temp = TempDir::new().unwrap();
    let source = source(&temp);
    let mut manager = manager(&temp);
    manager.init(&source).unwrap();
    let custom = temp.path().join("custom");
    let child = manager
        .create(
            Create::new(source.clone())
                .named("custom")
                .with_storage(Some(custom.clone())),
        )
        .unwrap();
    assert_eq!(child, fs::canonicalize(&custom).unwrap().join("custom"));
    assert!(matches!(
        manager.create(
            Create::new(source.clone())
                .named("custom")
                .with_storage(Some(custom))
        ),
        Err(Error::AlreadyExists(_))
    ));
    assert!(matches!(
        manager.create(Create::new(source.clone()).named("..")),
        Err(Error::Path(_))
    ));
    assert!(matches!(
        manager.create(
            Create::new(source.clone())
                .named("inside")
                .with_storage(Some(source.join("nested")))
        ),
        Err(Error::InsideSource(_))
    ));
    assert!(matches!(
        manager.create(Create::new(source.join("file.txt")).named("file")),
        Err(Error::Path(_))
    ));
}

#[test]
fn create_filters_regenerable_artifacts_by_default() {
    let temp = TempDir::new().unwrap();
    let source = source(&temp);
    fs::create_dir_all(source.join("node_modules/pkg")).unwrap();
    fs::write(source.join("node_modules/pkg/index.js"), "module").unwrap();
    fs::create_dir_all(source.join("target/debug")).unwrap();
    fs::write(source.join("target/debug/app"), "binary").unwrap();
    fs::create_dir_all(source.join(".yarn/cache")).unwrap();
    fs::write(source.join(".yarn/cache/pkg.zip"), "cache").unwrap();
    fs::write(source.join("package.json"), "{}").unwrap();
    fs::write(source.join("Cargo.lock"), "lock").unwrap();
    let mut manager = manager(&temp);
    manager.init(&source).unwrap();

    let child = manager
        .create(create_input(source.clone(), "filtered"))
        .unwrap();

    assert!(!child.join("node_modules").exists());
    assert!(!child.join("target").exists());
    assert!(!child.join(".yarn/cache").exists());
    assert_eq!(
        fs::read_to_string(child.join("package.json")).unwrap(),
        "{}"
    );
    assert_eq!(
        fs::read_to_string(child.join("Cargo.lock")).unwrap(),
        "lock"
    );
}

#[test]
fn create_preserves_tracked_files_in_filtered_directories() {
    let temp = TempDir::new().unwrap();
    let source = source(&temp);
    fs::create_dir_all(source.join("platform/coverage")).unwrap();
    fs::write(source.join("platform/coverage/module.py"), "tracked").unwrap();
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(&source)
            .args(["init", "--quiet"])
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(&source)
            .args(["add", "platform/coverage/module.py"])
            .status()
            .unwrap()
            .success()
    );
    let mut manager = manager(&temp);
    manager.init(&source).unwrap();

    let child = manager
        .create(create_input(source, "tracked-artifact"))
        .unwrap();

    assert_eq!(
        fs::read_to_string(child.join("platform/coverage/module.py")).unwrap(),
        "tracked"
    );
}

#[test]
fn create_copy_all_preserves_regenerable_artifacts() {
    let temp = TempDir::new().unwrap();
    let source = source(&temp);
    fs::create_dir_all(source.join("node_modules/pkg")).unwrap();
    fs::write(source.join("node_modules/pkg/index.js"), "module").unwrap();
    let mut manager = manager(&temp);
    manager.init(&source).unwrap();

    let child = manager
        .create_with_options(
            create_input(source.clone(), "copy-all"),
            create_options(CopyMode::All, HookMode::Run),
        )
        .unwrap();

    assert_eq!(
        fs::read_to_string(child.join("node_modules/pkg/index.js")).unwrap(),
        "module"
    );
}

#[test]
fn create_runs_postcreate_hooks_in_destination_order() {
    let temp = TempDir::new().unwrap();
    let source = source(&temp);
    fs::write(
        source.join(".rift.toml"),
        r#"
version = 1

[[hooks.postcreate]]
run = "echo first >> hook.log"

[[hooks.postcreate]]
run = "echo second >> hook.log"
"#,
    )
    .unwrap();
    let mut manager = manager(&temp);
    manager.init(&source).unwrap();

    let child = manager.create(create_input(source, "hooks")).unwrap();
    let log = fs::read_to_string(child.join("hook.log")).unwrap();

    assert!(log.find("first").unwrap() < log.find("second").unwrap());
}

#[test]
fn create_runs_precreate_before_copy_and_postcreate_after_copy() {
    let temp = TempDir::new().unwrap();
    let source = source(&temp);
    fs::write(
        source.join(".rift.toml"),
        r#"
version = 1

[[hooks.precreate]]
run = "echo pre >> lifecycle.log"

[[hooks.postcreate]]
run = "echo post >> lifecycle.log"
"#,
    )
    .unwrap();
    let mut manager = manager(&temp);
    manager.init(&source).unwrap();

    let child = manager
        .create(create_input(source.clone(), "lifecycle"))
        .unwrap();

    assert_eq!(
        fs::read_to_string(source.join("lifecycle.log")).unwrap(),
        "pre\n"
    );
    assert_eq!(
        fs::read_to_string(child.join("lifecycle.log")).unwrap(),
        "pre\npost\n"
    );
}

#[test]
fn precreate_failure_does_not_create_or_register_workspace() {
    let temp = TempDir::new().unwrap();
    let source = source(&temp);
    fs::write(
        source.join(".rift.toml"),
        "version = 1\n[[hooks.precreate]]\nrun = \"exit 7\"\n",
    )
    .unwrap();
    let mut manager = manager(&temp);
    manager.init(&source).unwrap();
    let expected = child_path(&source, "precreate-failure");

    let error = manager
        .create(create_input(source.clone(), "precreate-failure"))
        .unwrap_err();

    assert!(matches!(
        error,
        Error::HookFailed { hook, path, .. }
            if hook == "precreate" && path == source
    ));
    assert!(!expected.exists());
    assert!(manager.list(&source).unwrap().is_empty());
}

#[test]
fn postcreate_failure_leaves_registered_workspace() {
    let temp = TempDir::new().unwrap();
    let source = source(&temp);
    fs::write(
        source.join(".rift.toml"),
        r#"
version = 1

[[hooks.postcreate]]
run = "echo before >> hook.log"

[[hooks.postcreate]]
run = "exit 7"

[[hooks.postcreate]]
run = "echo after >> hook.log"
"#,
    )
    .unwrap();
    let mut manager = manager(&temp);
    manager.init(&source).unwrap();
    let expected = child_path(&source, "hook-failure");

    let error = manager
        .create(create_input(source.clone(), "hook-failure"))
        .unwrap_err();

    assert!(matches!(
        error,
        Error::HookFailed { path, .. } if path == expected
    ));
    assert!(expected.exists());
    assert_eq!(manager.list(&source).unwrap(), vec![expected.clone()]);
    let log = fs::read_to_string(expected.join("hook.log")).unwrap();
    assert!(log.contains("before"));
    assert!(!log.contains("after"));
}

#[test]
fn hook_skip_ignores_invalid_config() {
    let temp = TempDir::new().unwrap();
    let source = source(&temp);
    fs::write(source.join(".rift.toml"), "version = 2\n").unwrap();
    let mut manager = manager(&temp);
    manager.init(&source).unwrap();

    let child = manager
        .create_with_options(
            create_input(source, "skip-hooks"),
            create_options(CopyMode::Filtered, HookMode::Skip),
        )
        .unwrap();

    assert!(child.join(".rift.toml").exists());
}

#[test]
fn invalid_hook_config_fails_before_copying() {
    let temp = TempDir::new().unwrap();
    let source = source(&temp);
    fs::write(source.join(".rift.toml"), "version = 2\n").unwrap();
    let mut manager = manager(&temp);
    manager.init(&source).unwrap();
    let expected = child_path(&source, "invalid-config");

    let error = manager
        .create(create_input(source, "invalid-config"))
        .unwrap_err();

    assert!(matches!(error, Error::InvalidConfig { .. }));
    assert!(!expected.exists());
}

#[test]
fn corrupt_and_unknown_markers_are_rejected() {
    let temp = TempDir::new().unwrap();
    let source = source(&temp);
    let nested = source.join("nested");
    fs::create_dir(&nested).unwrap();
    let mut manager = manager(&temp);
    manager.init(&source).unwrap();
    fs::write(source.join(".rift"), "unknown\n").unwrap();
    assert!(matches!(
        manager.list(&nested),
        Err(Error::UnknownMarker(_))
    ));

    let id = manager.registry.record_at(&source).unwrap().unwrap().id;
    fs::write(source.join(".rift"), format!("{id}\n")).unwrap();
    let other = temp.path().join("other");
    fs::create_dir(&other).unwrap();
    fs::write(other.join(".rift"), format!("{id}\n")).unwrap();
    assert!(matches!(
        manager.list(&other),
        Err(Error::MarkerMismatch(_))
    ));
}

#[test]
fn removal_rejects_marker_mismatch_and_existing_trash_target() {
    let temp = TempDir::new().unwrap();
    let source = source(&temp);
    let mut manager = manager(&temp);
    manager.init(&source).unwrap();
    let child = manager
        .create(Create::new(source.clone()).named("child"))
        .unwrap();
    let id = marker_id(&child);
    fs::write(child.join(".rift"), "wrong\n").unwrap();
    assert!(matches!(
        manager.remove(&child),
        Err(Error::UnknownMarker(_))
    ));
    fs::write(
        child.join(".rift"),
        fs::read_to_string(source.join(".rift")).unwrap(),
    )
    .unwrap();
    assert!(matches!(
        manager.remove(&child),
        Err(Error::MarkerMismatch(_))
    ));
    fs::write(child.join(".rift"), format!("{id}\n")).unwrap();
    let trash = trash_path(&id, &child).unwrap();
    fs::create_dir_all(&trash).unwrap();
    assert!(matches!(
        manager.remove(&child),
        Err(Error::AlreadyExists(_))
    ));
}

#[test]
fn remove_runs_hooks_before_and_after_moving_workspace() {
    let temp = TempDir::new().unwrap();
    let source = source(&temp);
    let mut manager = manager(&temp);
    manager.init(&source).unwrap();
    let child = manager
        .create(Create::new(source).named("remove-hooks"))
        .unwrap();
    fs::write(
        child.join(".rift.toml"),
        r#"
version = 1

[[hooks.preremove]]
run = "echo pre >> lifecycle.log"

[[hooks.postremove]]
run = "echo post >> lifecycle.log"
"#,
    )
    .unwrap();
    let trash = trash_path(&marker_id(&child), &child).unwrap();

    manager.remove(&child).unwrap();

    assert!(!child.exists());
    assert_eq!(
        fs::read_to_string(trash.join("lifecycle.log")).unwrap(),
        "pre\npost\n"
    );
}

#[test]
fn preremove_failure_leaves_workspace_active() {
    let temp = TempDir::new().unwrap();
    let source = source(&temp);
    let mut manager = manager(&temp);
    manager.init(&source).unwrap();
    let child = manager
        .create(Create::new(source.clone()).named("remove-failure"))
        .unwrap();
    fs::write(
        child.join(".rift.toml"),
        "version = 1\n[[hooks.preremove]]\nrun = \"exit 9\"\n",
    )
    .unwrap();

    assert!(matches!(
        manager.remove(&child),
        Err(Error::HookFailed { hook, .. }) if hook == "preremove"
    ));
    assert!(child.exists());
    assert_eq!(manager.list(&source).unwrap(), vec![child]);
}

#[test]
fn gc_forgets_a_trashed_path_already_removed_on_disk() {
    let temp = TempDir::new().unwrap();
    let source = source(&temp);
    let mut manager = manager(&temp);
    manager.init(&source).unwrap();
    let child = manager.create(Create::new(source).named("child")).unwrap();
    let id = marker_id(&child);
    let trash = trash_path(&id, &child).unwrap();
    manager.remove(&child).unwrap();
    fs::remove_dir_all(&trash).unwrap();

    assert_eq!(manager.gc().unwrap(), vec![trash]);
}

#[test]
fn operations_use_the_nearest_ancestor_marker() {
    let temp = TempDir::new().unwrap();
    let source = source(&temp);
    let nested = source.join("packages/app");
    fs::create_dir_all(&nested).unwrap();
    let mut manager = manager(&temp);
    manager.init(&source).unwrap();

    let child = manager.create(Create::new(nested).named("nested")).unwrap();
    fs::create_dir(child.join("deep")).unwrap();

    assert_eq!(
        manager.list(source.join("packages")).unwrap(),
        vec![child.clone()]
    );
    assert_eq!(manager.ancestors(child.join("deep")).unwrap(), vec![source]);
}

#[test]
fn operations_without_a_marker_explain_how_to_initialize() {
    let temp = TempDir::new().unwrap();
    let source = source(&temp);
    let nested = source.join("nested");
    fs::create_dir(&nested).unwrap();
    let manager = manager(&temp);

    assert!(matches!(
        manager.list(&nested),
        Err(Error::WorkspaceNotInitialized(path)) if path == nested
    ));
}

#[test]
fn init_restores_a_deleted_marker_for_an_existing_workspace() {
    let temp = TempDir::new().unwrap();
    let source = source(&temp);
    let nested = source.join("nested");
    fs::create_dir(&nested).unwrap();
    let mut manager = manager(&temp);
    manager.init(&source).unwrap();
    let id = fs::read_to_string(source.join(".rift")).unwrap();
    fs::remove_file(source.join(".rift")).unwrap();

    assert!(matches!(
        manager.list(&nested),
        Err(Error::MissingMarker(path)) if path == source
    ));

    manager.init(&source).unwrap();
    assert_eq!(fs::read_to_string(source.join(".rift")).unwrap(), id);
    assert!(manager.list(&nested).unwrap().is_empty());
}

struct InitializingStrategy {
    initialized: Rc<Cell<bool>>,
}

impl Strategy for InitializingStrategy {
    fn copy_directory(&self, _from: &Path, _to: &Path, _mode: CopyMode) -> Result<()> {
        unreachable!()
    }

    fn initialize_directory(
        &self,
        _path: &Path,
        _progress: &mut dyn FnMut(InitProgress),
    ) -> Result<StrategyInit> {
        self.initialized.set(true);
        Ok(StrategyInit::Converted)
    }
}

#[test]
fn init_continues_initialization_after_restoring_a_marker() {
    let temp = TempDir::new().unwrap();
    let source = source(&temp);
    let mut registered = manager(&temp);
    registered.init(&source).unwrap();
    fs::remove_file(source.join(".rift")).unwrap();
    drop(registered);
    let initialized = Rc::new(Cell::new(false));
    let mut manager = Manager::with_strategy(
        temp.path().join("registry.sqlite"),
        Box::new(InitializingStrategy {
            initialized: initialized.clone(),
        }),
    )
    .unwrap();

    assert_eq!(manager.init(&source).unwrap(), InitOutcome::Converted);
    assert!(initialized.get());
    assert!(source.join(".rift").exists());
}

#[test]
fn init_registers_exactly_the_requested_directory() {
    let temp = TempDir::new().unwrap();
    let source = source(&temp);
    let nested = source.join("nested");
    fs::create_dir(&nested).unwrap();
    run(&source, &["init"]);
    let mut manager = manager(&temp);

    manager.init(&nested).unwrap();
    assert!(!source.join(".rift").exists());
    assert!(nested.join(".rift").exists());
}

#[test]
fn create_generates_readable_names_independent_of_ulid_identity() {
    let temp = TempDir::new().unwrap();
    let source = source(&temp);
    let mut manager = manager(&temp);
    manager.init(&source).unwrap();

    let destination = manager.create(Create::new(source)).unwrap();
    let name = destination.file_name().unwrap().to_str().unwrap();
    let parts = name.split('-').collect::<Vec<_>>();
    let id = fs::read_to_string(destination.join(".rift")).unwrap();
    let id = id.trim();

    assert_eq!(parts.len(), 2);
    assert!(
        parts[0]
            .chars()
            .all(|character| character.is_ascii_lowercase())
    );
    assert!(
        parts[1]
            .chars()
            .all(|character| character.is_ascii_lowercase())
    );
    assert!(Ulid::from_string(id).is_ok());
    assert_ne!(name, id);
}

#[test]
fn remove_trashes_a_full_subtree_and_gc_deletes_it() {
    let temp = TempDir::new().unwrap();
    let source = source(&temp);
    let mut manager = manager(&temp);
    manager.init(&source).unwrap();
    let first = manager
        .create(Create::new(source.clone()).named("first"))
        .unwrap();
    let second = manager
        .create(Create::new(first.clone()).named("second"))
        .unwrap();

    let first_id = marker_id(&first);
    let first_trash = trash_path(&first_id, &first).unwrap();
    let second_id = marker_id(&second);
    let second_trash = trash_path(&second_id, &second).unwrap();

    manager.remove(&first).unwrap();

    assert!(!first.exists());
    assert!(!second.exists());
    assert!(first_trash.exists());
    assert!(second_trash.exists());
    assert!(manager.list(&source).unwrap().is_empty());
    let deleted = manager.gc().unwrap();
    assert!(deleted.contains(&second_trash));
    assert!(deleted.contains(&first_trash));
    assert_eq!(deleted.len(), 2);
    assert!(!first_trash.exists());
    assert!(!second_trash.exists());
}

#[test]
fn remove_on_a_registered_root_unregisters_it_and_trashes_descendants() {
    let temp = TempDir::new().unwrap();
    let source = source(&temp);
    let mut manager = manager(&temp);
    manager.init(&source).unwrap();
    let first = manager
        .create(Create::new(source.clone()).named("first"))
        .unwrap();
    let second = manager
        .create(Create::new(first.clone()).named("second"))
        .unwrap();
    let first_id = marker_id(&first);
    let first_trash = trash_path(&first_id, &first).unwrap();
    let second_id = marker_id(&second);
    let second_trash = trash_path(&second_id, &second).unwrap();

    manager.remove(&source).unwrap();

    assert!(source.exists());
    assert!(!source.join(".rift").exists());
    assert!(!first.exists());
    assert!(!second.exists());
    assert!(first_trash.exists());
    assert!(second_trash.exists());
    assert!(matches!(
        manager.list(&source),
        Err(Error::WorkspaceNotInitialized(_))
    ));
    let deleted = manager.gc().unwrap();
    assert!(deleted.contains(&first_trash));
    assert!(deleted.contains(&second_trash));
}

#[test]
fn remove_on_a_registered_root_tolerates_missing_descendants() {
    let temp = TempDir::new().unwrap();
    let source = source(&temp);
    let mut manager = manager(&temp);
    manager.init(&source).unwrap();
    let first = manager
        .create(Create::new(source.clone()).named("first"))
        .unwrap();
    fs::remove_dir_all(&first).unwrap();

    manager.remove(&source).unwrap();

    assert!(source.exists());
    assert!(!source.join(".rift").exists());
    assert!(matches!(
        manager.list(&source),
        Err(Error::WorkspaceNotInitialized(_))
    ));
}

#[test]
fn remove_all_deletes_descendants_and_preserves_the_selected_workspace() {
    let temp = TempDir::new().unwrap();
    let source = source(&temp);
    let mut manager = manager(&temp);
    manager.init(&source).unwrap();
    let first = manager
        .create(Create::new(source.clone()).named("first"))
        .unwrap();
    let second = manager
        .create(Create::new(first.clone()).named("second"))
        .unwrap();
    let sibling = manager
        .create(Create::new(source.clone()).named("sibling"))
        .unwrap();
    let first_id = marker_id(&first);
    let first_trash = trash_path(&first_id, &first).unwrap();

    let removed = manager.remove_all(&source).unwrap();
    assert_eq!(removed[0], second);
    assert!(removed.contains(&first));
    assert!(removed.contains(&sibling));
    assert_eq!(removed.len(), 3);
    assert!(source.exists());
    assert!(!first.exists());
    assert!(first_trash.exists());
    assert!(manager.list(&source).unwrap().is_empty());
}

#[test]
fn remove_all_preserves_a_nested_selected_rift() {
    let temp = TempDir::new().unwrap();
    let source = source(&temp);
    let mut manager = manager(&temp);
    manager.init(&source).unwrap();
    let first = manager.create(Create::new(source).named("first")).unwrap();
    let second = manager
        .create(Create::new(first.clone()).named("second"))
        .unwrap();

    assert_eq!(manager.remove_all(&first).unwrap(), vec![second]);
    assert!(first.exists());
    assert!(manager.list(&first).unwrap().is_empty());
}

#[test]
fn remove_refuses_a_subtree_with_an_unlinked_move() {
    let temp = TempDir::new().unwrap();
    let source = source(&temp);
    let mut manager = manager(&temp);
    manager.init(&source).unwrap();
    let first = manager
        .create(Create::new(source.clone()).named("first"))
        .unwrap();
    let second = manager
        .create(Create::new(first.clone()).named("second"))
        .unwrap();
    fs::rename(&second, temp.path().join("moved")).unwrap();

    assert!(matches!(manager.remove(&first), Err(Error::MissingRift(_))));
    assert!(first.exists());
}

#[test]
fn gc_removes_trashed_entries() {
    let temp = TempDir::new().unwrap();
    let source = source(&temp);
    let mut manager = manager(&temp);
    manager.init(&source).unwrap();
    let first = manager
        .create(Create::new(source.clone()).named("first"))
        .unwrap();
    let second = manager
        .create(Create::new(first.clone()).named("second"))
        .unwrap();
    let first_id = marker_id(&first);
    let second_id = marker_id(&second);
    let first_trash = trash_path(&first_id, &first).unwrap();
    let second_trash = trash_path(&second_id, &second).unwrap();
    manager.remove(&first).unwrap();

    let deleted = manager.gc().unwrap();
    assert!(deleted.contains(&second_trash));
    assert!(deleted.contains(&first_trash));
    assert_eq!(deleted.len(), 2);
    assert!(manager.list(&source).unwrap().is_empty());
}

#[test]
fn gc_has_no_effect_on_active_rifts() {
    let temp = TempDir::new().unwrap();
    let source = source(&temp);
    let mut manager = manager(&temp);
    manager.init(&source).unwrap();
    let first = manager
        .create(Create::new(source.clone()).named("first"))
        .unwrap();
    let second = manager
        .create(Create::new(first.clone()).named("second"))
        .unwrap();
    assert!(manager.gc().unwrap().is_empty());
    assert_eq!(manager.ancestors(&second).unwrap(), vec![first, source]);
}

#[test]
fn gc_prunes_active_entries_deleted_outside_rift() {
    let temp = TempDir::new().unwrap();
    let source = source(&temp);
    let mut manager = manager(&temp);
    manager.init(&source).unwrap();
    let first = manager
        .create(Create::new(source.clone()).named("first"))
        .unwrap();
    let second = manager
        .create(Create::new(first.clone()).named("second"))
        .unwrap();
    fs::remove_dir_all(&first).unwrap();
    fs::remove_dir_all(&second).unwrap();

    let removed = manager.gc().unwrap();
    assert!(removed.contains(&first));
    assert!(removed.contains(&second));
    assert!(manager.list(&source).unwrap().is_empty());
}

#[test]
fn gc_preserves_missing_active_parent_with_an_existing_descendant() {
    let temp = TempDir::new().unwrap();
    let source = source(&temp);
    let mut manager = manager(&temp);
    manager.init(&source).unwrap();
    let first = manager
        .create(Create::new(source.clone()).named("first"))
        .unwrap();
    let second = manager
        .create(Create::new(first.clone()).named("second"))
        .unwrap();
    fs::remove_dir_all(&first).unwrap();

    assert!(manager.gc().unwrap().is_empty());
    assert_eq!(manager.list(&source).unwrap(), vec![first]);
    assert_eq!(
        manager.ancestors(&second).unwrap(),
        vec![source.parent().unwrap().join(".rifts/app/first"), source]
    );
}

#[test]
fn git_copy_detaches_head_and_preserves_dirty_state() {
    let temp = TempDir::new().unwrap();
    let source = source(&temp);
    run(&source, &["init"]);
    run(&source, &["config", "user.email", "test@example.com"]);
    run(&source, &["config", "user.name", "Test"]);
    run(&source, &["add", "file.txt"]);
    run(&source, &["commit", "-m", "initial"]);
    fs::write(source.join("file.txt"), "changed").unwrap();
    run(&source, &["add", "file.txt"]);
    fs::write(source.join("untracked.txt"), "new").unwrap();
    let mut manager = manager(&temp);
    manager.init(&source).unwrap();

    let destination = manager
        .create(Create::new(source.clone()).named("git"))
        .unwrap();

    let source_commit = Command::new("git")
        .arg("-C")
        .arg(&source)
        .args(["rev-parse", "--verify", "HEAD^{commit}"])
        .output()
        .unwrap();
    assert!(
        !Command::new("git")
            .arg("-C")
            .arg(&destination)
            .args(["symbolic-ref", "-q", "HEAD"])
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(
        fs::read_to_string(destination.join(".git/HEAD")).unwrap(),
        format!(
            "{}\n",
            String::from_utf8_lossy(&source_commit.stdout).trim()
        )
    );
    let staged = Command::new("git")
        .arg("-C")
        .arg(&destination)
        .args(["diff", "--cached", "--name-only"])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&staged.stdout).contains("file.txt"));
    assert!(destination.join("untracked.txt").exists());
    let status = Command::new("git")
        .arg("-C")
        .arg(&destination)
        .args(["status", "--porcelain", "--", ".rift"])
        .output()
        .unwrap();
    assert!(status.stdout.is_empty());
}

#[test]
fn git_copy_peels_symbolic_tag_heads_to_commits() {
    let temp = TempDir::new().unwrap();
    let source = source(&temp);
    run(&source, &["init"]);
    run(&source, &["config", "user.email", "test@example.com"]);
    run(&source, &["config", "user.name", "Test"]);
    run(&source, &["add", "file.txt"]);
    run(&source, &["commit", "-m", "initial"]);
    run(&source, &["tag", "-a", "release", "-m", "release"]);
    run(&source, &["symbolic-ref", "HEAD", "refs/tags/release"]);
    let expected = Command::new("git")
        .arg("-C")
        .arg(&source)
        .args(["rev-parse", "--verify", "HEAD^{commit}"])
        .output()
        .unwrap();
    let mut manager = manager(&temp);
    manager.init(&source).unwrap();

    let destination = manager
        .create(Create::new(source).named("tag-head"))
        .unwrap();

    assert_eq!(
        fs::read_to_string(destination.join(".git/HEAD")).unwrap(),
        format!("{}\n", String::from_utf8_lossy(&expected.stdout).trim())
    );
}

#[test]
fn create_requires_an_initialized_workspace() {
    let temp = TempDir::new().unwrap();
    let source = source(&temp);
    let mut manager = manager(&temp);

    assert!(matches!(
        manager.create(Create::new(source.clone()).named("unsafe")),
        Err(Error::WorkspaceNotInitialized(_))
    ));
    assert!(!source.join(".rift").exists());
}

#[test]
fn unsafe_git_states_are_rejected_after_initialization() {
    let temp = TempDir::new().unwrap();
    let source = source(&temp);
    run(&source, &["init"]);
    let mut manager = manager(&temp);
    manager.init(&source).unwrap();

    for state in [
        "MERGE_HEAD",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "BISECT_LOG",
        "rebase-merge",
        "rebase-apply",
        "index.lock",
        "HEAD.lock",
    ] {
        let marker = source.join(".git").join(state);
        if state.starts_with("rebase-") {
            fs::create_dir(&marker).unwrap();
        } else {
            fs::write(&marker, "commit").unwrap();
        }
        let name = format!("unsafe-{state}");
        let expected = child_path(&source, &name);

        let error = manager
            .create(Create::new(source.clone()).named(name))
            .unwrap_err();

        assert!(matches!(error, Error::UnsafeGit(message) if message.contains(state)));
        assert!(!expected.exists());
        assert!(manager.list(&source).unwrap().is_empty());
        if marker.is_dir() {
            fs::remove_dir(&marker).unwrap();
        } else {
            fs::remove_file(&marker).unwrap();
        }
    }
}

#[test]
fn linked_git_worktree_source_is_rejected_after_initialization() {
    let temp = TempDir::new().unwrap();
    let source = source(&temp);
    run(&source, &["init"]);
    let mut manager = manager(&temp);
    manager.init(&source).unwrap();
    fs::remove_dir_all(source.join(".git")).unwrap();
    fs::write(source.join(".git"), "gitdir: ../linked/.git").unwrap();

    let error = manager
        .create(Create::new(source.clone()).named("linked-worktree"))
        .unwrap_err();

    assert!(matches!(error, Error::UnsafeGit(message) if message.contains("linked")));
    assert!(!child_path(&source, "linked-worktree").exists());
    assert!(manager.list(&source).unwrap().is_empty());
}

struct PartialFailureStrategy;

impl Strategy for PartialFailureStrategy {
    fn copy_directory(&self, _from: &Path, to: &Path, _mode: CopyMode) -> Result<()> {
        fs::create_dir(to)?;
        fs::write(to.join("copied-before-failure.txt"), "partial")?;
        fs::create_dir(to.join("nested"))?;
        fs::write(to.join("nested/file.txt"), "partial")?;
        Err(Error::CowUnavailable("partial failure".into()))
    }
}

#[test]
fn partial_copy_failure_removes_child_and_registry_row() {
    let temp = TempDir::new().unwrap();
    let source = source(&temp);
    let mut manager = Manager::with_strategy(
        temp.path().join("registry.sqlite"),
        Box::new(PartialFailureStrategy),
    )
    .unwrap();
    manager.init(&source).unwrap();
    let expected = child_path(&source, "partial");

    let error = manager
        .create(Create::new(source.clone()).named("partial"))
        .unwrap_err();

    assert!(matches!(error, Error::CowUnavailable(message) if message == "partial failure"));
    assert!(source.join(".rift").exists());
    assert!(!expected.exists());
    assert!(manager.list(&source).unwrap().is_empty());
}

#[cfg(unix)]
#[test]
fn unreadable_source_file_failure_removes_child_and_registry_row() {
    if running_as_root() {
        return;
    }
    let temp = TempDir::new().unwrap();
    let source = source(&temp);
    let secret = source.join("secret.txt");
    fs::write(&secret, "secret").unwrap();
    let mut manager = manager(&temp);
    manager.init(&source).unwrap();
    fs::set_permissions(&secret, fs::Permissions::from_mode(0o000)).unwrap();

    let result = manager.create(Create::new(source.clone()).named("unreadable-file"));
    fs::set_permissions(&secret, fs::Permissions::from_mode(0o600)).unwrap();
    let error = result.unwrap_err();

    assert!(matches!(error, Error::Io(_)));
    assert!(source.join(".rift").exists());
    assert!(!child_path(&source, "unreadable-file").exists());
    assert!(manager.list(&source).unwrap().is_empty());
}

#[cfg(unix)]
#[test]
fn unreadable_source_directory_failure_removes_child_and_registry_row() {
    if running_as_root() {
        return;
    }
    let temp = TempDir::new().unwrap();
    let source = source(&temp);
    let secret = source.join("secret");
    fs::create_dir(&secret).unwrap();
    fs::write(secret.join("file.txt"), "secret").unwrap();
    let mut manager = manager(&temp);
    manager.init(&source).unwrap();
    fs::set_permissions(&secret, fs::Permissions::from_mode(0o000)).unwrap();

    let result = manager.create(Create::new(source.clone()).named("unreadable-directory"));
    fs::set_permissions(&secret, fs::Permissions::from_mode(0o700)).unwrap();
    let error = result.unwrap_err();

    assert!(matches!(error, Error::Walk(_) | Error::Io(_)));
    assert!(source.join(".rift").exists());
    assert!(!child_path(&source, "unreadable-directory").exists());
    assert!(manager.list(&source).unwrap().is_empty());
}

#[cfg(unix)]
#[test]
fn unwritable_destination_parent_failure_leaves_no_child_or_registry_row() {
    if running_as_root() {
        return;
    }
    let temp = TempDir::new().unwrap();
    let source = source(&temp);
    let parent = temp.path().join("readonly");
    fs::create_dir(&parent).unwrap();
    let mut manager = manager(&temp);
    manager.init(&source).unwrap();
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o500)).unwrap();

    let result = manager.create(
        Create::new(source.clone())
            .named("blocked")
            .with_storage(Some(parent.clone())),
    );
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o700)).unwrap();
    let error = result.unwrap_err();

    assert!(matches!(error, Error::Io(_)));
    assert!(source.join(".rift").exists());
    assert!(!parent.join("blocked").exists());
    assert!(manager.list(&source).unwrap().is_empty());
}

#[test]
fn unavailable_cow_does_not_create_a_child() {
    let temp = TempDir::new().unwrap();
    let source = source(&temp);
    let mut manager = Manager::with_strategy(
        temp.path().join("registry.sqlite"),
        Box::new(FailureStrategy),
    )
    .unwrap();
    manager.init(&source).unwrap();

    assert!(matches!(
        manager.create(Create::new(source.clone()).named("failure")),
        Err(Error::CowUnavailable(_))
    ));
    assert!(source.join(".rift").exists());
    assert!(manager.list(&source).unwrap().is_empty());
}

#[cfg(unix)]
fn running_as_root() -> bool {
    // SAFETY: geteuid has no preconditions and only reads the process identity.
    unsafe { libc::geteuid() == 0 }
}

fn run(path: &Path, args: &[&str]) {
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(path)
            .args(args)
            .status()
            .unwrap()
            .success()
    );
}
