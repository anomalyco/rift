#![cfg(windows)]

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::{Builder, TempDir};

#[test]
fn refs_environment_can_create_a_workspace() {
    let Some(fixture) = refs_fixture() else {
        return;
    };
    let source = prepared_source(&fixture);
    fixture.success(&source, ["init", "--here"]);
    let create = fixture.run(&source, ["create", "--name", "child"]);
    assert!(
        create.status.success(),
        "RIFT_REQUIRE_REFS_TESTS requires create to succeed on this volume\n{}",
        command_text(&create)
    );
    println!("windows e2e: create succeeded on {}", source.display());
}

#[test]
fn init_and_create_print_paths_without_verbatim_prefix() {
    let Some(fixture) = refs_fixture() else {
        return;
    };
    let source = prepared_source(&fixture);
    let init = fixture.success(&source, ["init", "--here"]);
    assert!(init.stdout.is_empty());
    assert_no_verbatim_prefix(&init.stderr);

    let create = fixture.success(&source, ["create", "--name", "child"]);
    let child = create.single_stdout_path();
    assert_no_verbatim_prefix(&create.stdout);
    assert_no_verbatim_prefix(&create.stderr);
    assert!(child.join("marker.txt").is_file());
    println!(
        "windows e2e: init and create printed {} without a verbatim prefix",
        child.display()
    );
}

#[test]
fn remove_from_inside_the_workspace_moves_it_to_trash() {
    let Some(fixture) = refs_fixture() else {
        return;
    };
    let (_source, child) = created_child(&fixture);
    let remove = fixture.run(&child, ["remove"]);
    assert!(
        remove.status.success(),
        "remove from inside the workspace failed\n{}",
        command_text(&remove)
    );
    assert!(!child.exists());
    assert!(trash_contains_marker(child.parent().unwrap()));
    println!(
        "windows e2e: remove from inside moved {} to trash",
        child.display()
    );
}

#[test]
fn powershell_remove_returns_to_the_parent_workspace() {
    let Some(fixture) = refs_fixture() else {
        return;
    };
    require_pwsh();
    let (source, child) = created_child(&fixture);
    let parent = fixture.success(&child, ["ancestors"]).single_stdout_path();
    let init_script = fixture.success(fixture.root(), ["shell-init", "pwsh"]);
    let script = format!(
        "{init}\nSet-Location -LiteralPath {child}\nrift --database {database} remove\nif ($LASTEXITCODE -ne 0) {{ exit $LASTEXITCODE }}\nWrite-Output (Get-Location).ProviderPath\n",
        init = init_script.stdout,
        child = powershell_literal(&child),
        database = powershell_literal(fixture.database()),
    );
    let output = Command::new("pwsh")
        .args(["-NoProfile", "-Command", &script])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "pwsh remove failed\n{}",
        command_text(&output)
    );
    let location = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .next_back()
        .unwrap_or("")
        .trim()
        .to_owned();
    assert!(
        same_directory(&location, &parent.to_string_lossy()),
        "expected to end in {}, got {location}",
        parent.display()
    );
    assert_no_verbatim_prefix(&location);
    assert!(!child.exists());
    assert!(source.join(".rift").is_file());
    println!("windows e2e: pwsh remove returned to {}", parent.display());
}

fn refs_fixture() -> Option<Fixture> {
    std::env::var_os("RIFT_REQUIRE_REFS_TESTS")
        .is_some()
        .then(Fixture::new)
}

fn require_pwsh() {
    let available = Command::new("pwsh")
        .args(["-NoProfile", "-Command", "exit 0"])
        .status()
        .is_ok_and(|status| status.success());
    assert!(available, "RIFT_REQUIRE_REFS_TESTS requires pwsh on PATH");
}

fn prepared_source(fixture: &Fixture) -> PathBuf {
    let source = fixture.root().join("source");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("marker.txt"), "kept").unwrap();
    source
}

fn created_child(fixture: &Fixture) -> (PathBuf, PathBuf) {
    let source = prepared_source(fixture);
    fixture.success(&source, ["init", "--here"]);
    let child = fixture
        .success(&source, ["create", "--name", "child"])
        .single_stdout_path();
    (source, child)
}

fn trash_contains_marker(storage: &Path) -> bool {
    let trash = storage.join(".trash");
    std::fs::read_dir(&trash).is_ok_and(|entries| {
        entries
            .filter_map(Result::ok)
            .any(|entry| entry.path().join("marker.txt").is_file())
    })
}

fn assert_no_verbatim_prefix(text: &str) {
    assert!(!text.contains(r"\\?\"), "{text}");
}

fn same_directory(left: &str, right: &str) -> bool {
    fn normalized(path: &str) -> String {
        path.trim()
            .trim_start_matches(r"\\?\")
            .trim_end_matches(['\\', '/'])
            .to_ascii_lowercase()
    }
    normalized(left) == normalized(right)
}

fn powershell_literal(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "''"))
}

struct Fixture {
    temp: TempDir,
    database: PathBuf,
    binary: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = Builder::new()
            .prefix(".rift-cli-windows-")
            .tempdir_in(std::env::current_dir().unwrap())
            .unwrap();
        Self {
            database: temp.path().join("registry.sqlite"),
            binary: PathBuf::from(env!("CARGO_BIN_EXE_rift")),
            temp,
        }
    }

    fn root(&self) -> &Path {
        self.temp.path()
    }

    fn database(&self) -> &Path {
        &self.database
    }

    fn success<I, S>(&self, cwd: &Path, args: I) -> CommandOutput
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let output = self.run(cwd, args);
        assert!(
            output.status.success(),
            "expected success\n{}",
            command_text(&output)
        );
        command_output(output)
    }

    fn run<I, S>(&self, cwd: &Path, args: I) -> Output
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        Command::new(&self.binary)
            .arg("--database")
            .arg(&self.database)
            .args(args)
            .current_dir(cwd)
            .output()
            .unwrap()
    }
}

struct CommandOutput {
    stdout: String,
    stderr: String,
}

impl CommandOutput {
    fn single_stdout_path(&self) -> PathBuf {
        let paths = self.stdout.lines().map(PathBuf::from).collect::<Vec<_>>();
        assert_eq!(paths.len(), 1, "expected one stdout path, got {paths:?}");
        paths.into_iter().next().unwrap()
    }
}

fn command_output(output: Output) -> CommandOutput {
    CommandOutput {
        stdout: String::from_utf8(output.stdout).unwrap(),
        stderr: String::from_utf8(output.stderr).unwrap(),
    }
}

fn command_text(output: &Output) -> String {
    format!(
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}
